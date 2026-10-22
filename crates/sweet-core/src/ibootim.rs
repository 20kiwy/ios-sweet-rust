//! iBootIm container + SSZL ("lzss") decoder — the format of the all_flash
//! boot assets (applelogo, glyphcharging, ...) inside iOS IPSWs.
//!
//! Header (64 bytes, per the `ibootim` reference implementation):
//!
//! ```text
//! char    signature[8]   "iBootIm\0"
//! u32     adler          header checksum (not verified here)
//! u32     compression    'lzss' (file bytes "sszl") or 'none'
//! u32     colorSpace     'grey' (file bytes "yerg"), 'ARGB' / 'BGRA'
//! u16     width
//! u16     height
//! i16     offsetX
//! i16     offsetY
//! u32     compressedSize
//! u32     reserved[8]
//! ````
//!
//! The compressed pixel stream follows at offset 64. SSZL is Apple's classic
//! LZSS from iBoot: 4096-byte ring buffer, the first N-F bytes initialized
//! to spaces (0x20), write cursor r = N-F; flag bytes consumed LSB-first
//! (bit 1 = literal); a match pair b0/b1 encodes the ABSOLUTE window base
//! `i = b0 | (b1 & 0xf0) << 4` and length `j = (b1 & 0x0f) + 2`, copied
//! `j + 1` times from `text_buf[(i + k) & 4095]`.
//!
//! The 'grey' color space stores darkness (255 = background/transparent,
//! 0 = fully drawn); the boot logo composites as white where the mask is
//! dark, so we invert into RGBA luminance.

use crate::png::Image;
use crate::{bail, Result};

pub const MAGIC: &[u8; 8] = b"iBootIm\0";
const HEADER_SIZE: usize = 64;

pub struct IBootImage {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub offset_x: i16,
    pub offset_y: i16,
    /// Decoded RGBA8 pixels (logo drawn white-on-black).
    pub rgba: Vec<u8>,
}

pub fn looks_ibootim(buf: &[u8]) -> bool {
    buf.len() > HEADER_SIZE && &buf[..8] == MAGIC
}

fn fourcc(bytes: &[u8]) -> String {
    let mut v: Vec<u8> = bytes.to_vec();
    v.reverse();
    String::from_utf8_lossy(&v).to_ascii_lowercase()
}

/// Decode an iBootIm payload (after IM4P unwrap) to RGBA.
pub fn decode(buf: &[u8]) -> Result<IBootImage> {
    if !looks_ibootim(buf) {
        bail!("not an iBootIm image");
    }
    let compression = fourcc(&buf[12..16]); // "lzss" or "none"
    let format = fourcc(&buf[16..20]); // "grey", "argb", "bgra"
    let width = u16::from_le_bytes([buf[20], buf[21]]) as u32;
    let height = u16::from_le_bytes([buf[22], buf[23]]) as u32;
    let offset_x = i16::from_le_bytes([buf[24], buf[25]]);
    let offset_y = i16::from_le_bytes([buf[26], buf[27]]);
    let compressed_size =
        u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]) as usize;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        bail!("implausible iBootIm size {width}x{height}");
    }
    let expect_raw = (width * height) as usize * pxlen(&format);
    let pixels = match compression.as_str() {
        "none" => buf[HEADER_SIZE..].to_vec(),
        "lzss" => {
            let end = (HEADER_SIZE + compressed_size).min(buf.len());
            sszl_decode(&buf[HEADER_SIZE..end], expect_raw)?
        }
        other => bail!("unsupported iBootIm compression {other}"),
    };
    let expect = (width * height) as usize * pxlen(&format);
    if pixels.len() < expect {
        bail!("iBootIm decoded {} bytes, expected {}", pixels.len(), expect_raw);
    }

    let mut rgba = Vec::with_capacity((width * height) as usize * 4);
    match format.as_str() {
        // darkness mask: 255 = background, 0 = logo drawn (white on device)
        "grey" => {
            for &v in &pixels[..(width * height) as usize] {
                let l = 255 - v;
                rgba.extend_from_slice(&[l, l, l, 255]);
            }
        }
        "argb" => {
            for px in pixels[..expect].chunks(4) {
                rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
            }
        }
        "bgra" => {
            for px in pixels[..expect].chunks(4) {
                rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        other => bail!("unsupported iBootIm pixel format {other}"),
    }
    Ok(IBootImage { format, width, height, offset_x, offset_y, rgba })
}

fn pxlen(format: &str) -> usize {
    if format == "grey" {
        1
    } else {
        4
    }
}

/// Apple iBoot LZSS ("sszl" in the file, 'lzss' as the stored fourcc).
pub fn sszl_decode(src: &[u8], dst_size: usize) -> Result<Vec<u8>> {
    const N: usize = 4096;
    const F: usize = 18;
    const THRESHOLD: usize = 2;
    let mut text_buf = vec![0x20u8; N + F - 1]; // spaces, like the reference
    let mut r = N - F;
    let mut out = Vec::with_capacity(dst_size);
    let mut i = 0usize;
    let mut flags: u32 = 0;
    while out.len() < dst_size {
        flags >>= 1;
        if flags & 0x100 == 0 {
            if i >= src.len() {
                break;
            }
            flags = src[i] as u32 | 0xff00;
            i += 1;
        }
        if flags & 1 != 0 {
            if i >= src.len() {
                break;
            }
            let c = src[i];
            i += 1;
            out.push(c);
            text_buf[r] = c;
            r = (r + 1) & (N - 1);
        } else {
            if i + 1 >= src.len() {
                break;
            }
            let a = src[i] as usize | ((src[i + 1] as usize & 0xf0) << 4);
            let j = (src[i + 1] as usize & 0x0f) + THRESHOLD;
            i += 2;
            for k in 0..=j {
                if out.len() >= dst_size {
                    break;
                }
                let c = text_buf[(a + k) & (N - 1)];
                out.push(c);
                text_buf[r] = c;
                r = (r + 1) & (N - 1);
            }
        }
    }
    Ok(out)
}

impl IBootImage {
    pub fn to_image(&self) -> Image {
        Image { width: self.width, height: self.height, rgba: self.rgba.clone() }
    }
}

/// Trivial SSZL *encoder* for tests: all-literal streams are valid.
pub fn sszl_encode_literals(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 1);
    for chunk in data.chunks(8) {
        let mut flags = 0u8;
        for (b, _) in chunk.iter().enumerate() {
            flags |= 1 << b;
        }
        out.push(flags);
        out.extend_from_slice(chunk);
    }
    out
}

/// Build a synthetic iBootIm (grey) for tests.
pub fn encode_grey(width: u16, height: u16, darkness: &[u8]) -> Vec<u8> {
    let stream = sszl_encode_literals(darkness);
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&0x5357_4545u32.to_le_bytes()); // adler stand-in
    out.extend_from_slice(b"sszl"); // 'lzss' fourcc (stored byte-reversed)
    out.extend_from_slice(b"yerg"); // 'grey' fourcc (stored byte-reversed)
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&0i16.to_le_bytes()); // offset_x
    out.extend_from_slice(&0i16.to_le_bytes()); // offset_y
    out.extend_from_slice(&(stream.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 32]); // reserved[8]
    out.extend_from_slice(&stream);
    out
}
