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
pub const LC_LOAD_DYLIB: u32 = 0xc;
pub const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018; // (0x18 | LC_REQ_DYLD)
pub const LC_LOAD_UPWARD_DYLIB: u32 = 0x8000_0023; // (0x23 | LC_REQ_DYLD)
pub const LC_REEXPORT_DYLIB: u32 = 0x8000_001f; // (0x1f | LC_REQ_DYLD)
pub const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
pub const LC_FILESET_ENTRY: u32 = 0x8000_0035;
pub const MH_MAGIC_64: u32 = 0xfeedfacf;
pub const MH_CIGAM_FAT: u32 = 0xbeba_feca; // FAT_MAGIC 0xcafebabe, read LE
pub const CPU_TYPE_ARM64: u32 = 0x0100_000c;
pub const CPU_TYPE_ARM64E: u32 = 0x0200_000c;

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub segname: String,
    pub addr: u64,
    pub size: u64,
    pub offset: u32,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub name: String,
    pub vmaddr: u64,
    pub vmsize: u64,
    pub fileoff: u64,
    pub filesize: u64,
    pub sections: Vec<Section>,
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
    /// Linked libraries: LC_LOAD_DYLIB / LC_LOAD_WEAK_DYLIB / LC_REEXPORT_DYLIB.
    pub dylibs: Vec<String>,
    /// LC_FILESET_ENTRY entries (kernel collections).
    pub fileset: Vec<FilesetEntry>,
}

#[derive(Debug, Clone)]
pub struct FilesetEntry {
    pub id: String,
    pub vmaddr: u64,
    pub fileoff: u64,
}

/// (offset, size, cputype) of the arm64 slice of a FAT Mach-O (thin files
/// return (0, len, cputype)). iOS userspace binaries are FAT; A11 userspace
/// is plain arm64 (arm64e starts at A12).
pub fn fat_slice_arm64(buf: &[u8]) -> Result<(usize, usize, u32)> {
    if buf.len() < 8 || u32le(buf, 0) != MH_CIGAM_FAT {
        // thin: must still be a 64-bit Mach-O to accept it
        if buf.len() >= 4 && u32le(buf, 0) == MH_MAGIC_64 {
            return Ok((0, buf.len(), u32le(buf, 4)));
        }
        bail!("not a FAT or 64-bit Mach-O");
    }
    let be32 = |off: usize| -> u32 { u32le(buf, off).swap_bytes() };
    let narch = be32(4) as usize;
    let mut arm64e = None;
    for i in 0..narch {
        let e = 8 + i * 20;
        if e + 20 > buf.len() {
            bail!("FAT arch entry out of bounds");
        }
        let (cpu, off, size) = (be32(e), be32(e + 8) as usize, be32(e + 12) as usize);
        if off + size > buf.len() {
            bail!("FAT slice out of bounds");
        }
        if cpu == CPU_TYPE_ARM64 {
            return Ok((off, size, cpu));
        }
        if cpu == CPU_TYPE_ARM64E && arm64e.is_none() {
            arm64e = Some((off, size, cpu));
        }
    }
    arm64e.ok_or_else(|| crate::Error::Msg("no arm64 slice in FAT Mach-O".into()))
}

/// Parse the arm64 slice of a (FAT or thin) Mach-O; returns (macho, slice offset).
pub fn parse_best(buf: &[u8]) -> Result<(Macho, usize)> {
    let (off, size, _) = fat_slice_arm64(buf)?;
    Ok((parse(&buf[off..off + size])?, off))
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
    let mut m = Macho {
        cputype,
        cpusub,
        segments: Vec::new(),
        entry: 0,
        fixups: None,
        dylibs: Vec::new(),
        fileset: Vec::new(),
    };
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
                let nsects = u32le(buf, off + 64) as usize;
                let mut sections = Vec::new();
                for i in 0..nsects {
                    let so = off + 72 + i * 80;
                    if so + 80 > buf.len() {
                        bail!("section header out of bounds");
                    }
                    let sname: String = buf[so..so + 16]
                        .iter()
                        .take_while(|&&c| c != 0)
                        .map(|&c| c as char)
                        .collect();
                    let sgname: String = buf[so + 16..so + 32]
                        .iter()
                        .take_while(|&&c| c != 0)
                        .map(|&c| c as char)
                        .collect();
                    sections.push(Section {
                        name: sname,
                        segname: sgname,
                        addr: u64le(buf, so + 32),
                        size: u64le(buf, so + 40),
                        offset: u32le(buf, so + 48),
                    });
                }
                m.segments.push(Segment {
                    name,
                    vmaddr: u64le(buf, off + 24),
                    vmsize: u64le(buf, off + 32),
                    fileoff: u64le(buf, off + 40),
                    filesize: u64le(buf, off + 48),
                    sections,
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
            LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_LOAD_UPWARD_DYLIB | LC_REEXPORT_DYLIB => {
                // dylib_command: cmd, cmdsize, name offset (from cmd start), ...
                if size < 12 {
                    bail!("short LC_*_DYLIB");
                }
                let name_off = off + u32le(buf, off + 8) as usize;
                if name_off >= off + size || name_off >= buf.len() {
                    bail!("dylib name offset out of bounds");
                }
                let name: String = buf[name_off..off + size]
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as char)
                    .collect();
                if !name.is_empty() {
                    m.dylibs.push(name);
                }
            }
            LC_FILESET_ENTRY => {
                // fileset_entry_command: cmd, cmdsize, vmaddr, fileoff,
                // entry_id (string offset), reserved
                if size < 32 {
                    bail!("short LC_FILESET_ENTRY");
                }
                let id_off = off + u32le(buf, off + 24) as usize;
                if id_off >= buf.len() || id_off >= off + size {
                    bail!("fileset entry_id offset out of bounds");
                }
                let id: String = buf[id_off..off + size]
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as char)
                    .collect();
                m.fileset.push(FilesetEntry {
                    id,
                    vmaddr: u64le(buf, off + 8),
                    fileoff: u64le(buf, off + 16),
                });
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

/// Every DEFINED symbol of the arm64 slice as `(name, value)` — the native
/// `llvm-nm --defined-only` parity (W2: the external tool was a sandbox/CI
/// trap; the kernel's LC_SYMTAB is right there and we already parse Mach-O).
///
/// Only `n_type & N_TYPE != 0` entries are reported (defined absolute /
/// section / prebound-undefined-with-value); pure debug (`N_SO`/`N_FUN`) and
/// undefined entries are skipped, matching `nm --defined-only` output that
/// `extract_font`/`screen-decode` previously consumed.
pub fn symbols(buf: &[u8]) -> Result<Vec<(String, u64)>> {
    const LC_SYMTAB: u32 = 0x2;
    const N_TYPE: u8 = 0x0e;
    let (off, size, _) = fat_slice_arm64(buf)?;
    let s = &buf[off..off + size];
    if s.len() < 32 || u32le(s, 0) != MH_MAGIC_64 {
        bail!("not a 64-bit Mach-O");
    }
    let ncmds = u32le(s, 16) as usize;
    let mut symoff = 0usize;
    let mut nsyms = 0usize;
    let mut stroff = 0usize;
    let mut strsize = 0usize;
    let mut p = 32usize;
    for _ in 0..ncmds {
        if p + 8 > s.len() {
            bail!("truncated load command at {p:#x}");
        }
        let cmd = u32le(s, p);
        let cmdsize = u32le(s, p + 4) as usize;
        if cmdsize < 8 || p + cmdsize > s.len() {
            bail!("bad load command size {cmdsize} at {p:#x}");
        }
        if cmd == LC_SYMTAB && cmdsize >= 24 {
            symoff = u32le(s, p + 8) as usize;
            nsyms = u32le(s, p + 12) as usize;
            stroff = u32le(s, p + 16) as usize;
            strsize = u32le(s, p + 20) as usize;
            break;
        }
        p += cmdsize;
    }
    if nsyms == 0 || stroff == 0 || symoff == 0 {
        bail!("arm64 slice has no LC_SYMTAB (stripped kernel?)");
    }
    let strtab_end = (stroff + strsize).min(s.len());
    let mut out = Vec::new();
    for i in 0..nsyms {
        let e = symoff + i * 16;
        if e + 16 > s.len() {
            break;
        }
        let n_type = s[e + 4];
        if n_type & N_TYPE == 0 {
            continue; // undefined or debug entry (local defined keep: N_TYPE!=0)
        }
        let n_strx = u32le(s, e) as usize;
        if n_strx == 0 || stroff + n_strx >= strtab_end {
            continue;
        }
        let name_end = s[stroff + n_strx..strtab_end]
            .iter()
            .position(|&c| c == 0)
            .map(|p| stroff + n_strx + p)
            .unwrap_or(strtab_end);
        let name = std::str::from_utf8(&s[stroff + n_strx..name_end]).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        out.push((name.to_string(), u64le(s, e + 8)));
    }
    Ok(out)
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
