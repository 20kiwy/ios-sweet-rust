//! 64-bit Mach-O parsing and LC_DYLD_CHAINED_FIXUPS rebasing.
//!
//! Port of `parse_macho()` / `apply_chained_fixups()` / `decode()` in
//! `tools/mkpayload.py`. The SWEET xnu links as a PIE with chained fixups;
//! nobody else resolves them (no kmutil, no iBoot), so the payload builder
//! walks the chains and rewrites every pointer to its unslid target.

use crate::aarch64::{u32le, u64le};
use crate::{bail, Result};

pub const LC_SEGMENT_64: u32 = 0x19;
pub const LC_UNIXTHREAD: u32 = 0x5;
pub const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
pub const MH_MAGIC_64: u32 = 0xfeedfacf;
pub const CPU_TYPE_ARM64: u32 = 0x0100_000c;

#[derive(Debug, Clone)]
pub struct Segment {
    pub name: String,
    pub vmaddr: u64,
    pub vmsize: u64,
    pub fileoff: u64,
    pub filesize: u64,
}

#[derive(Debug, Clone)]
pub struct Macho {
    pub cputype: u32,
    pub cpusub: u32,
    pub segments: Vec<Segment>,
    /// LC_UNIXTHREAD pc (entry point VA).
    pub entry: u64,
    /// (dataoff, datasize) of LC_DYLD_CHAINED_FIXUPS, if present.
    pub fixups: Option<(u32, u32)>,
}

/// Little-endian u16 at `off`.
fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}

pub fn parse(buf: &[u8]) -> Result<Macho> {
    if buf.len() < 32 || u32le(buf, 0) != MH_MAGIC_64 {
        bail!("not a 64-bit Mach-O");
    }
    let cputype = u32le(buf, 4);
    let cpusub = u32le(buf, 8) & 0xff;
    let ncmds = u32le(buf, 16) as usize;
    let mut m = Macho { cputype, cpusub, segments: Vec::new(), entry: 0, fixups: None };
    let mut off = 32usize;
    for _ in 0..ncmds {
        if off + 8 > buf.len() {
            bail!("truncated load command at {off:#x}");
        }
        let cmd = u32le(buf, off);
        let size = u32le(buf, off + 4) as usize;
        if size < 8 || off + size > buf.len() {
            bail!("bad load command size {size} at {off:#x}");
        }
        match cmd {
            LC_SEGMENT_64 => {
                if size < 72 {
                    bail!("short LC_SEGMENT_64");
                }
                let name = buf[off + 8..off + 24]
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as char)
                    .collect::<String>();
                m.segments.push(Segment {
                    name,
                    vmaddr: u64le(buf, off + 24),
                    vmsize: u64le(buf, off + 32),
                    fileoff: u64le(buf, off + 40),
                    filesize: u64le(buf, off + 48),
                });
            }
            LC_UNIXTHREAD => {
                // flavor, count, then arm_thread_state64: x0..x28, fp, lr, sp, pc, cpsr
                // (33 u64 registers + cpsr/pad u32s = 272 state bytes; cmd = 288)
                if size < 16 + 33 * 8 + 8 {
                    bail!("short LC_UNIXTHREAD");
                }
                m.entry = u64le(buf, off + 16 + 32 * 8);
            }
            LC_DYLD_CHAINED_FIXUPS => {
                if size < 16 {
                    bail!("short LC_DYLD_CHAINED_FIXUPS");
                }
                m.fixups = Some((u32le(buf, off + 8), u32le(buf, off + 12)));
            }
            _ => {}
        }
        off += size;
    }
    if m.segments.is_empty() {
        bail!("Mach-O has no segments");
    }
    Ok(m)
}

/// Rewritten pointer count returned alongside the mutated buffer.
pub fn apply_chained_fixups(buf: &mut [u8], m: &Macho) -> Result<usize> {
    let Some((data_off, _)) = m.fixups else { return Ok(0) };
    let data_off = data_off as usize;
    if data_off + 28 > buf.len() {
        bail!("chained fixups header out of bounds");
    }
    let starts_off = u32le(buf, data_off + 4) as usize;
    let imports_count = u32le(buf, data_off + 16);
    if imports_count != 0 {
        bail!("kernel has binds; only rebases are supported");
    }
    let image_base = m.segments[0].vmaddr; // __TEXT, holds the mach header
    let seg_count = u32le(buf, data_off + starts_off) as usize;
    let mut nfix = 0usize;
    for i in 0..seg_count {
        let seg_info = u32le(buf, data_off + starts_off + 4 + 4 * i);
        if seg_info == 0 {
            continue;
        }
        let base = data_off + starts_off + seg_info as usize;
        // Layout (<IHHQIH at base): u32 size, u16 page_size, u16 fmt,
        // u32 seg_offset, u32 max_valid, u32 page_count, then u16 starts[].
        if base + 22 > buf.len() {
            bail!("chained fixups seg info out of bounds");
        }
        let page_size = u16le(buf, base + 4) as usize;
        let fmt = u16le(buf, base + 6) as u64;
        let seg_offset = u32le(buf, base + 8) as u64;
        let page_count = u32le(buf, base + 20) as usize;
        if base + 22 + 2 * page_count > buf.len() {
            bail!("chained fixups page starts out of bounds");
        }
        let seg = m
            .segments
            .get(i)
            .ok_or_else(|| crate::Error::Msg(format!("fixup seg index {i} out of range")))?;
        for p in 0..page_count {
            let start = u16le(buf, base + 22 + 2 * p) as usize;
            if start == 0xffff {
                continue;
            }
            if start & 0x8000 != 0 {
                bail!("DYLD_CHAINED_PTR_START_MULTI not supported");
            }
            let mut loc = seg.fileoff as usize + p * page_size + start;
            loop {
                if loc + 8 > buf.len() {
                    bail!("chained fixup pointer out of bounds at {loc:#x}");
                }
                let raw = u64le(buf, loc);
                let (target, next, stride) = decode(raw, fmt, image_base)?;
                crate::aarch64::put_u64le(buf, loc, target);
                nfix += 1;
                if next == 0 {
                    break;
                }
                loc += next as usize * stride as usize;
            }
        }
        let _ = seg_offset; // kept for documentation parity; offsets come from the segment
    }
    Ok(nfix)
}

/// (unslid target VA, next, stride) for one chained pointer — port of `decode()`.
fn decode(raw: u64, fmt: u64, image_base: u64) -> Result<(u64, u32, u32)> {
    match fmt {
        2 | 6 => {
            // DYLD_CHAINED_PTR_64 / _64_OFFSET
            if raw >> 63 != 0 {
                bail!("bind in kernel");
            }
            let mut target = raw & ((1 << 36) - 1);
            let high8 = (raw >> 36) & 0xff;
            let next = ((raw >> 51) & 0xfff) as u32;
            if fmt == 6 {
                target = target.wrapping_add(image_base);
            }
            Ok((target | (high8 << 56), next, 4))
        }
        8 => {
            // DYLD_CHAINED_PTR_64_KERNEL_CACHE
            let target = raw & ((1 << 30) - 1);
            let next = ((raw >> 51) & 0xfff) as u32;
            Ok((image_base.wrapping_add(target), next, 4))
        }
        1 | 7 | 9 | 12 => {
            // arm64e variants (stock VMAPPLE kernel only; PAC is not re-signed)
            let auth = raw >> 63 != 0;
            let next = ((raw >> 51) & 0x7ff) as u32;
            let stride = if fmt == 7 || fmt == 12 { 4 } else { 8 };
            let mut target = if auth {
                raw & 0xffff_ffff
            } else {
                let t = raw & ((1 << 43) - 1);
                t | (((raw >> 43) & 0xff) << 56)
            };
            if (7..=9).contains(&fmt) || fmt == 12 || auth {
                target = target.wrapping_add(image_base);
            }
            Ok((target, next, stride))
        }
        other => bail!("unsupported chained pointer format {other}"),
    }
}
