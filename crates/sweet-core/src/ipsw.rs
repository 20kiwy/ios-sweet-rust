//! IPSW (Apple's zip) reader: local files and remote URL via HTTP range
//! requests through `curl` — the Rust port/replacement of `tools/remotezip.py`
//! plus plain zip extraction. Handles ZIP64 (the 16.7.16 iPhone X IPSW is
//! ~6 GB, far beyond the 4 GiB zip limit).

use crate::{bail, Result};
use std::io::{Read, Seek};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct IpswEntry {
    pub name: String,
    pub size: u64,
    pub comp_size: u64,
    pub method: u16,
    pub offset: u64, // absolute offset of the local header
}

impl IpswEntry {
    pub fn data_offset(&self) -> u64 {
        // local header: sig(4) + ver(2) + flags(2) + method(2) + time(2) + date(2)
        // + crc(4) + comp(4) + uncomp(4) + name_len(2) + extra_len(2) = 30
        // (the real header field lengths are re-read at extract time)
        self.offset + 30 + self.name.len() as u64
    }
}

#[derive(Debug, Clone, Default)]
pub struct IpswIndex {
    pub entries: Vec<IpswEntry>,
}

pub enum Source {
    File(std::fs::File),
    Url(String),
}

pub fn open(source: &str) -> Result<Source> {
    if source.starts_with("http://") || source.starts_with("https://") {
        Ok(Source::Url(source.to_string()))
    } else {
        Ok(Source::File(std::fs::File::open(source)?))
    }
}

fn curl_range(url: &str, start: u64, len: u64) -> Result<Vec<u8>> {
    let out = Command::new("curl")
        .args([
            "-sSfL",
            "--retry",
            "3",
            "--range",
            &format!("{start}-{}", start + len - 1),
            url,
        ])
        .output()
        .map_err(|e| crate::Error::Msg(format!("curl: {e}")))?;
    if !out.status.success() {
        bail!(
            "curl range {start}-{} failed: {}",
            start + len - 1,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

fn url_size(url: &str) -> Result<u64> {
    let out = Command::new("curl")
        .args(["-sSfIL", "--retry", "3", url])
        .output()
        .map_err(|e| crate::Error::Msg(format!("curl -I: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let l = line.trim().to_ascii_lowercase();
        if let Some(v) = l.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<u64>() {
                return Ok(n);
            }
        }
    }
    bail!("no Content-Length for {url}");
}

const EOCD: u32 = 0x0605_4b50;
const EOCD64_LOC: u32 = 0x0706_4b50;
const EOCD64: u32 = 0x0606_4b50;
const CD_ENTRY: u32 = 0x0201_4b50;

/// Locate + parse the central directory. `read_tail(off, len)` supplies bytes
/// (local file or curl range), keeping the reader transport-agnostic.
fn find_cd<F>(total: u64, mut read_tail: F) -> Result<(u64, u64)>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>>,
{
    // Search back for the EOCD (max comment 64K + 22).
    let scan = (total.min(66_559)).max(22);
    let tail = read_tail(total - scan, scan)?;
    let mut eocd_pos = None;
    for i in (0..tail.len().saturating_sub(21)).rev() {
        if u32le(&tail, i) == EOCD {
            eocd_pos = Some(total - scan + i as u64);
            break;
        }
    }
    let Some(eocd_pos) = eocd_pos else { bail!("no zip EOCD") };
    let mut cd_off = u32le(&tail, (eocd_pos - (total - scan) + 16) as usize) as u64;
    let mut cd_size = u32le(&tail, (eocd_pos - (total - scan) + 12) as usize) as u64;
    // ZIP64: locator is right before the EOCD.
    if eocd_pos >= 20 {
        let loc = read_tail(eocd_pos - 20, 20)?;
        if u32le(&loc, 0) == EOCD64_LOC {
            let zip64_eocd_off = u64le(&loc, 8);
            let hdr = read_tail(zip64_eocd_off, 56)?;
            if u32le(&hdr, 0) == EOCD64 {
                cd_size = u64le(&hdr, 40);
                cd_off = u64le(&hdr, 48);
            }
        }
    }
    Ok((cd_off, cd_size))
}

pub fn index(source: &str) -> Result<IpswIndex> {
    match open(source)? {
        Source::File(mut f) => {
            let total = f.metadata()?.len();
            let (cd_off, cd_size) = find_cd(total, |off, len| {
                f.seek(std::io::SeekFrom::Start(off))?;
                let mut b = vec![0u8; len as usize];
                f.read_exact(&mut b)?;
                Ok(b)
            })?;
            f.seek(std::io::SeekFrom::Start(cd_off))?;
            let mut cd = vec![0u8; cd_size as usize];
            f.read_exact(&mut cd)?;
            parse_cd(&cd)
        }
        Source::Url(url) => {
            let total = url_size(&url)?;
            let (cd_off, cd_size) = find_cd(total, |off, len| curl_range(&url, off, len))?;
            let cd = curl_range(&url, cd_off, cd_size)?;
            parse_cd(&cd)
        }
    }
}

fn parse_cd(cd: &[u8]) -> Result<IpswIndex> {
    let mut entries = Vec::new();
    let mut off = 0usize;
    while off + 46 <= cd.len() && u32le(cd, off) == CD_ENTRY {
        let flags = u16le(cd, off + 8);
        let method = u16le(cd, off + 10);
        let mut comp = u32le(cd, off + 20) as u64;
        let mut uncomp = u32le(cd, off + 24) as u64;
        let name_len = u16le(cd, off + 28) as usize;
        let extra_len = u16le(cd, off + 30) as usize;
        let comment_len = u16le(cd, off + 32) as usize;
        let mut offset = u32le(cd, off + 42) as u64;
        let name = String::from_utf8_lossy(&cd[off + 46..off + 46 + name_len]).to_string();
        // ZIP64 extended information extra field (id 0x0001): sizes/offset in
        // the same order they appear in the fixed record, only for 0xFFFFFFFF.
        let extra = &cd[off + 46 + name_len..off + 46 + name_len + extra_len];
        let mut eoff = 0usize;
        while eoff + 4 <= extra.len() {
            let id = u16le(extra, eoff);
            let sz = u16le(extra, eoff + 2) as usize;
            let f = &extra[eoff + 4..(eoff + 4 + sz).min(extra.len())];
            if id == 0x0001 {
                let mut fo = 0usize;
                if uncomp == 0xffff_ffff && fo + 8 <= f.len() {
                    uncomp = u64le(f, fo);
                    fo += 8;
                }
                if comp == 0xffff_ffff && fo + 8 <= f.len() {
                    comp = u64le(f, fo);
                    fo += 8;
                }
                if offset == 0xffff_ffff && fo + 8 <= f.len() {
                    offset = u64le(f, fo);
                }
            }
            eoff += 4 + sz;
        }
        entries.push(IpswEntry {
            name: if flags & 0x800 != 0 { name } else { name }, // utf-8 best effort
            size: uncomp,
            comp_size: comp,
            method,
            offset,
        });
        off += 46 + name_len + extra_len + comment_len;
    }
    if entries.is_empty() {
        bail!("no central directory entries");
    }
    Ok(IpswIndex { entries })
}

/// Extract one member. Works for STORE and DEFLATE (miniz inflate).
pub fn extract(source: &str, entry: &IpswEntry) -> Result<Vec<u8>> {
    // Re-read the local header for the true name/extra lengths.
    let (name_len, extra_len) = match open(source)? {
        Source::File(mut f) => {
            f.seek(std::io::SeekFrom::Start(entry.offset))?;
            let mut h = [0u8; 30];
            f.read_exact(&mut h)?;
            if u32le(&h, 0) != 0x0403_4b50 {
                bail!("bad local header for {}", entry.name);
            }
            (
                u16le(&h, 26) as u64,
                u16le(&h, 28) as u64,
            )
        }
        Source::Url(url) => {
            let h = curl_range(&url, entry.offset, 30)?;
            if u32le(&h, 0) != 0x0403_4b50 {
                bail!("bad local header for {}", entry.name);
            }
            (u16le(&h, 26) as u64, u16le(&h, 28) as u64)
        }
    };
    let data_off = entry.offset + 30 + name_len + extra_len;
    let raw = match open(source)? {
        Source::File(mut f) => {
            f.seek(std::io::SeekFrom::Start(data_off))?;
            let mut b = vec![0u8; entry.comp_size as usize];
            f.read_exact(&mut b)?;
            b
        }
        Source::Url(url) => curl_range(&url, data_off, entry.comp_size)?,
    };
    match entry.method {
        0 => {
            if raw.len() as u64 != entry.size {
                bail!("stored size mismatch for {}", entry.name);
            }
            Ok(raw)
        }
        8 => {
            let mut dst = vec![0u8; entry.size as usize];
            raw_inflate(&raw, &mut dst)?; // zip members are RAW deflate
            Ok(dst)
        }
        m => bail!("unsupported zip compression method {m} for {}", entry.name),
    }
}

extern "C" {
    fn mz_uncompress2(
        dest: *mut u8,
        dest_len: *mut core::ffi::c_ulong,
        source: *const u8,
        source_len: *mut core::ffi::c_ulong,
    ) -> i32;
    fn mz_inflateInit2(stream: *mut MzStream, window_bits: i32) -> i32;
    fn mz_inflate(stream: *mut MzStream, flush: i32) -> i32;
    fn mz_inflateEnd(stream: *mut MzStream) -> i32;
    fn mz_compress2(
        dest: *mut u8,
        dest_len: *mut core::ffi::c_ulong,
        source: *const u8,
        source_len: core::ffi::c_ulong,
        level: i32,
    ) -> i32;
    fn mz_compressBound(source_len: core::ffi::c_ulong) -> core::ffi::c_ulong;
    fn mz_crc32(crc: core::ffi::c_ulong, ptr: *const u8, len: u64) -> core::ffi::c_ulong;
}

/// miniz mz_stream layout (verbatim field order from miniz.h).
#[repr(C)]
struct MzStream {
    next_in: *const u8,
    avail_in: core::ffi::c_uint,
    total_in: core::ffi::c_ulong,
    next_out: *mut u8,
    avail_out: core::ffi::c_uint,
    total_out: core::ffi::c_ulong,
    msg: *mut u8,
    #[allow(dead_code)]
    state: *mut core::ffi::c_void,
    // NULL zalloc/zfree -> miniz uses its malloc/free defaults
    zalloc: Option<unsafe extern "C" fn(*mut core::ffi::c_void, usize, usize) -> *mut core::ffi::c_void>,
    zfree: Option<unsafe extern "C" fn(*mut core::ffi::c_void, *mut core::ffi::c_void)>,
    #[allow(dead_code)]
    opaque: *mut core::ffi::c_void,
    #[allow(dead_code)]
    data_type: i32,
    #[allow(dead_code)]
    adler: core::ffi::c_ulong,
    #[allow(dead_code)]
    reserved: core::ffi::c_ulong,
}

/// RAW-deflate inflate (zip member data has no zlib wrapper).
fn raw_inflate(src: &[u8], dst: &mut [u8]) -> Result<()> {
    let mut s = MzStream {
        next_in: src.as_ptr(),
        avail_in: src.len() as core::ffi::c_uint,
        total_in: 0,
        next_out: dst.as_mut_ptr(),
        avail_out: dst.len() as core::ffi::c_uint,
        total_out: 0,
        msg: std::ptr::null_mut(),
        state: std::ptr::null_mut(),
        zalloc: None,
        zfree: None,
        opaque: std::ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    };
    let rc = unsafe { mz_inflateInit2(&mut s, -15) }; // -15 = raw
    if rc != 0 {
        bail!("raw inflate init failed (rc={rc})");
    }
    // MZ_OK (0) or MZ_STREAM_END (1) both mean success with MZ_FINISH
    let rc = unsafe { mz_inflate(&mut s, 4 /* MZ_FINISH */) };
    let out = s.total_out as usize;
    unsafe { mz_inflateEnd(&mut s) };
    if rc < 0 {
        bail!("raw inflate failed (rc={rc}, out={out})");
    }
    if out != dst.len() {
        bail!("raw inflate short: {out} != {}", dst.len());
    }
    Ok(())
}

/// zlib inflate into a pre-sized buffer (miniz).
pub fn inflate_into(src: &[u8], dst: &mut [u8]) -> Result<()> {
    let mut dst_len = dst.len() as core::ffi::c_ulong;
    let mut src_len = src.len() as core::ffi::c_ulong;
    let rc = unsafe {
        mz_uncompress2(dst.as_mut_ptr(), &mut dst_len, src.as_ptr(), &mut src_len)
    };
    if rc != 0 {
        bail!("inflate failed (miniz rc={rc})");
    }
    Ok(())
}

pub fn crc32(data: &[u8]) -> u32 {
    unsafe { mz_crc32(0, data.as_ptr(), data.len() as u64) as u32 }
}

/// zlib deflate at `level` (miniz) — used by the PNG encoder.
pub fn zlib_compress(data: &[u8], level: i32) -> Result<Vec<u8>> {
    let bound = unsafe { mz_compressBound(data.len() as core::ffi::c_ulong) } as usize;
    let mut dst = vec![0u8; bound + 16];
    let mut dst_len = dst.len() as core::ffi::c_ulong;
    let rc = unsafe {
        mz_compress2(
            dst.as_mut_ptr(),
            &mut dst_len,
            data.as_ptr(),
            data.len() as core::ffi::c_ulong,
            level,
        )
    };
    if rc != 0 {
        bail!("deflate failed (miniz rc={rc})");
    }
    dst.truncate(dst_len as usize);
    Ok(dst)
}

pub fn save(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, data)?;
    Ok(())
}

// little-endian helpers local to this module
fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}
fn u64le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}
