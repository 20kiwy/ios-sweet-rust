//! PNG encode/decode and framebuffer composition.
//!
//! Encoder is the port of `qemu-sweet.py write_png()` (RGB, row filter 0).
//! Decoder supports what Apple's all_flash assets use: 8/16-bit gray, RGB,
//! palette (+tRNS), gray+alpha, RGBA, filters 0-4, non-interlaced.
//! Inflate/deflate/CRC32 come from the vendored miniz (via `ipsw` FFI).

use crate::ipsw::{crc32, inflate_into, zlib_compress};
use crate::{bail, Result};

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major.
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Image {
        Image { width, height, rgba: vec![0; width as usize * height as usize * 4] }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let o = (y as usize * self.width as usize + x as usize) * 4;
        [self.rgba[o], self.rgba[o + 1], self.rgba[o + 2], self.rgba[o + 3]]
    }
}

// ---------------------------------------------------------------------------
// Encode (RGB PNG, like write_png in qemu-sweet.py)
// ---------------------------------------------------------------------------

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Encode an RGB PNG from packed RGB8 data (3 bytes/px).
pub fn encode_rgb(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>> {
    if rgb.len() != width as usize * height as usize * 3 {
        bail!("rgb buffer is {} bytes, expected {}", rgb.len(), width as usize * height as usize * 3);
    }
    let mut raw = Vec::with_capacity(height as usize * (width as usize * 3 + 1));
    for y in 0..height as usize {
        raw.push(0); // filter None
        let s = y * width as usize * 3;
        raw.extend_from_slice(&rgb[s..s + width as usize * 3]);
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut png, b"IHDR", &ihdr);
    let z = zlib_compress(&raw, 6)?;
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

/// Encode an RGBA framebuffer (a8r8g8b8 little-endian, i.e. B G R A in
/// memory) as an RGB PNG — exact port of `write_png()` for the QEMU fb dump.
pub fn encode_fb(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>> {
    let stride = width as usize * 4;
    if bgra.len() < height as usize * stride {
        bail!("fb buffer is {} bytes, expected {}", bgra.len(), height as usize * stride);
    }
    let mut rgb = Vec::with_capacity(height as usize * width as usize * 3);
    for y in 0..height as usize {
        let line = &bgra[y * stride..(y + 1) * stride];
        for x in 0..width as usize {
            rgb.push(line[x * 4 + 2]); // R
            rgb.push(line[x * 4 + 1]); // G
            rgb.push(line[x * 4]);     // B
        }
    }
    encode_rgb(width, height, &rgb)
}

// ---------------------------------------------------------------------------
// Decode (all_flash applelogo assets and any PNG the pipeline sees)
// ---------------------------------------------------------------------------

pub fn decode(data: &[u8]) -> Result<Image> {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        bail!("not a PNG");
    }
    let mut off = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut depth = 8u8;
    let mut color = 6u8;
    let mut interlace = 0u8;
    let mut plte: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut seen_ihdr = false;
    while off + 8 <= data.len() {
        let len = u32be(data, off) as usize;
        let kind = &data[off + 4..off + 8];
        let body = data
            .get(off + 8..off + 8 + len)
            .ok_or_else(|| crate::Error::Msg("PNG chunk out of bounds".into()))?;
        match kind {
            b"IHDR" => {
                if body.len() < 13 {
                    bail!("short IHDR");
                }
                width = u32be(body, 0);
                height = u32be(body, 4);
                depth = body[8];
                color = body[9];
                interlace = body[12];
                seen_ihdr = true;
            }
            b"PLTE" => {
                plte = body.chunks(3).map(|c| [c[0], c.get(1).copied().unwrap_or(0), c.get(2).copied().unwrap_or(0)]).collect();
            }
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        off += 12 + len;
    }
    if !seen_ihdr {
        bail!("PNG without IHDR");
    }
    if interlace != 0 {
        bail!("Adam7-interlaced PNG not supported (not used by Apple assets)");
    }
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        bail!("implausible PNG size {width}x{height}");
    }
    let channels: usize = match (color, depth) {
        (0, 8) => 1, // gray
        (0, 16) => 1,
        (2, 8) => 3, // rgb
        (2, 16) => 3,
        (3, 8) => 1, // palette
        (4, 8) => 2, // gray + alpha
        (6, 8) => 4, // rgba
        (6, 16) => 4,
        (c, d) => bail!("unsupported PNG color type {c} depth {d}"),
    };
    let mut raw = vec![0u8; height as usize * (width as usize * channels + 1)];
    inflate_into(&idat, &mut raw)?;
    #[cfg(feature = "png_debug")]
    eprintln!("PNG debug: idat={} raw[0..16]={:?}", idat.len(), &raw[..16.min(raw.len())]);

    let stride = width as usize * channels;
    let bpp = channels.max(1); // filter unit bytes
    let mut img = Image::new(width, height);
    let mut prev = vec![0u8; stride];
    for y in 0..height as usize {
        let row = &raw[y * (stride + 1) + 1..y * (stride + 1) + 1 + stride];
        let ft = raw[y * (stride + 1)];
        let mut cur = row.to_vec();
        for x in 0..stride {
            let a = if x >= bpp { cur[x - bpp] } else { 0 };
            let b = prev[x];
            let c = if x >= bpp { prev[x - bpp] } else { 0 };
            cur[x] = match ft {
                0 => cur[x],
                1 => cur[x].wrapping_add(a),
                2 => cur[x].wrapping_add(b),
                3 => cur[x].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let pa = (p - a as i16).abs();
                    let pb = (p - b as i16).abs();
                    let pc = (p - c as i16).abs();
                    let pred = if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c };
                    cur[x].wrapping_add(pred)
                }
                f => bail!("bad PNG filter {f}"),
            };
        }
        // cur: channel-ordered samples -> RGBA
        for x in 0..width as usize {
            let g = |i: usize| -> u8 {
                if depth == 16 {
                    cur[x * channels + i * 2] // big end of the 16-bit sample
                } else {
                    cur[x * channels + i]
                }
            };
            let o = (y * width as usize + x) * 4;
            match color {
                0 => {
                    let v = g(0);
                    img.rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
                }
                2 => img.rgba[o..o + 4].copy_from_slice(&[g(0), g(1), g(2), 255]),
                3 => {
                    let idx = cur[x * channels] as usize;
                    let p = plte.get(idx).ok_or_else(|| {
                        crate::Error::Msg(format!("palette index {idx} out of range"))
                    })?;
                    let a = trns.get(idx).copied().unwrap_or(255);
                    img.rgba[o..o + 4].copy_from_slice(&[p[0], p[1], p[2], a]);
                }
                4 => {
                    let v = g(0);
                    img.rgba[o..o + 4].copy_from_slice(&[v, v, v, g(1)]);
                }
                6 => img.rgba[o..o + 4].copy_from_slice(&[g(0), g(1), g(2), g(3)]),
                _ => unreachable!(),
            }
        }
        prev.copy_from_slice(&cur);
    }
    Ok(img)
}

fn u32be(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}
