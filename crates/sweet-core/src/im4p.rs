//! IM4P / DER parsing and LZFSE decompression — port of the DER half of
//! `tools/adt.py`, with Apple's LZFSE C implementation vendored (FFI).

use crate::{bail, Result};

/// DER element at `off` -> (tag, value_start, value_end).
fn der(buf: &[u8], off: usize) -> Result<(u8, usize, usize)> {
    if off + 2 > buf.len() {
        bail!("DER truncated at {off:#x}");
    }
    let tag = buf[off];
    let mut ln = buf[off + 1] as usize;
    let mut off = off + 2;
    if ln & 0x80 != 0 {
        let n = ln & 0x7f;
        if n == 0 || n > 8 || off + n > buf.len() {
            bail!("bad DER long length");
        }
        ln = buf[off..off + n].iter().fold(0usize, |a, &b| (a << 8) | b as usize);
        off += n;
    }
    if off + ln > buf.len() {
        bail!("DER value out of bounds");
    }
    Ok((tag, off, off + ln))
}

#[derive(Debug, Clone)]
pub struct Im4pInfo {
    pub kind: String,
    pub desc: String,
    pub encrypted: bool,
}

extern "C" {
    fn lzfse_decode_buffer(
        dst: *mut u8,
        dst_size: usize,
        src: *const u8,
        src_size: usize,
        scratch: *mut u8,
    ) -> usize;
    fn lzfse_encode_scratch_size() -> usize;
    fn lzfse_encode_buffer(
        dst: *mut u8,
        dst_size: usize,
        src: *const u8,
        src_size: usize,
        scratch: *mut u8,
    ) -> usize;
}


/// LZFSE decode with the expected output size known up front (blkx chunks).
pub fn lzfse_exact(src: &[u8], expect: usize) -> Result<Vec<u8>> {
    let mut dst = vec![0u8; expect + 64];
    let n = unsafe {
        lzfse_decode_buffer(dst.as_mut_ptr(), dst.len(), src.as_ptr(), src.len(), std::ptr::null_mut())
    };
    if n == 0 || n > expect {
        bail!("lzfse_exact: decoded {n}, expected {expect}");
    }
    dst.truncate(n);
    Ok(dst)
}

/// LZFSE decode with an output-size doubling loop (bvx2/bvxn/bvn/bvlzvn blocks).
pub fn lzfse_decode(src: &[u8]) -> Result<Vec<u8>> {
    let mut dst_size = (src.len() as u64).saturating_mul(4).max(1 << 20) as usize;
    loop {
        let mut dst = vec![0u8; dst_size];
        let n = unsafe {
            lzfse_decode_buffer(dst.as_mut_ptr(), dst.len(), src.as_ptr(), src.len(), std::ptr::null_mut())
        };
        if n > 0 && n < dst.len() {
            dst.truncate(n);
            return Ok(dst);
        }
        if n == dst.len() {
            // Maybe exactly this size; verify by trying to decode into n+1.
            let mut probe = vec![0u8; dst.len() + 1];
            let m = unsafe {
                lzfse_decode_buffer(
                    probe.as_mut_ptr(),
                    probe.len(),
                    src.as_ptr(),
                    src.len(),
                    std::ptr::null_mut(),
                )
            };
            if m > 0 && m < probe.len() {
                probe.truncate(m);
                return Ok(probe);
            }
            if dst_size >= 1 << 30 {
                bail!("lzfse output exceeds 1 GiB");
            }
            dst_size *= 2;
            continue;
        }
        bail!("lzfse_decode_buffer failed (n={n})");
    }
}

/// LZFSE encode (used by tests to round-trip through real Apple C code).
pub fn lzfse_encode(src: &[u8]) -> Result<Vec<u8>> {
    let scratch_size = unsafe { lzfse_encode_scratch_size() };
    let mut scratch = vec![0u8; scratch_size.max(1)];
    let mut dst = vec![0u8; src.len() + src.len() / 4 + 64];
    let n = unsafe {
        lzfse_encode_buffer(dst.as_mut_ptr(), dst.len(), src.as_ptr(), src.len(), scratch.as_mut_ptr())
    };
    if n == 0 {
        bail!("lzfse_encode_buffer failed");
    }
    dst.truncate(n);
    Ok(dst)
}

/// Unwrap an IM4P: returns (payload, info). Decompresses bvx2/bvx1/bvn
/// LZFSE payloads. Port of `adt.py im4p_payload()` + `load()`.
pub fn unwrap(buf: &[u8]) -> Result<(Vec<u8>, Im4pInfo)> {
    if buf.first() != Some(&0x30) {
        bail!("not a DER sequence");
    }
    let (tag, s, e) = der(buf, 0)?;
    if tag != 0x30 {
        bail!("not a DER sequence");
    }
    let mut items = Vec::new();
    let mut off = s;
    while off < e {
        let (t, vs, ve) = der(buf, off)?;
        items.push((t, &buf[vs..ve]));
        off = ve;
    }
    if items.len() < 4 || items[0].1 != b"IM4P".as_slice() {
        bail!("not an IM4P (items[0] = {:?})", items.first().map(|x| x.1));
    }
    let kind = String::from_utf8_lossy(items[1].1).to_string();
    let desc = String::from_utf8_lossy(items[2].1).to_string();
    // Optional trailing items: KBAG (OCTET STRING, tag 0x04) = encrypted,
    // SEQUENCE (0x30) = compression info.
    let encrypted = items.iter().skip(4).any(|(t, _)| *t == 0x04);
    let info = Im4pInfo { kind, desc, encrypted };
    if info.encrypted {
        bail!("payload is encrypted (KBAG present)");
    }
    let mut payload = items[3].1.to_vec();
    if matches!(payload.get(0..4), Some(b"bvx2") | Some(b"bvx1") | Some(b"bvxn")) {
        payload = lzfse_decode(&payload)?;
    }
    Ok((payload, info))
}

/// True when the buffer looks like an IM4P.
pub fn looks_im4p(buf: &[u8]) -> bool {
    buf.first() == Some(&0x30) && buf[..16.min(buf.len())].windows(4).any(|w| w == b"IM4P")
}
