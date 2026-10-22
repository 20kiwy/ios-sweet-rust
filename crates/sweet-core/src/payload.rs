//! SWEETXNU payload builder and QEMU RAM-image placement.
//!
//! `build()` is the port of `tools/mkpayload.py::build()`;
//! `build_image()` / `stub()` / `payload_data()` are ports of
//! `tools/qemu-sweet.py` (what `shim/loader.c` does on the phone).

use crate::aarch64::{align, put_u32le, put_u64le, u32le, u64le};
use crate::macho;
use crate::mkadt::{add_ramdisk_to_adt, adt_dram};
use crate::{bail, FB_BASE, FB_H, FB_STRIDE, FB_W, PAGE, Result};

pub const MAGIC: &[u8; 8] = b"SWEETXNU";
pub const VERSION: u32 = 1;
pub const BOOT_LINE_LENGTH: usize = 608;
pub const CMDLINE: usize = 0x48;
pub const SEG_TABLE: usize = CMDLINE + BOOT_LINE_LENGTH; // 0x2a8

#[derive(Debug, Clone)]
pub struct BuildInfo {
    pub cputype: u32,
    pub cpusub: u32,
    pub fixups: usize,
    pub virt_base: u64,
    pub entry: u64,
    pub kernel_end: u64,
    pub ramdisk: Option<(u64, u64)>,
    pub size: usize,
    pub segments: Vec<(String, u64, u64)>,
}

impl BuildInfo {
    pub fn summary(&self) -> String {
        let segs: Vec<String> =
            self.segments.iter().map(|(n, a, s)| format!("{n} {}+{}", hexs(*a), hexs(*s))).collect();
        format!(
            "cputype={} cpusub={} fixups={} virt_base={} entry={} kernel_end={} ramdisk={} size={} segs=[{}]",
            hexs(self.cputype as u64),
            self.cpusub,
            self.fixups,
            hexs(self.virt_base),
            hexs(self.entry),
            hexs(self.kernel_end),
            self.ramdisk
                .map(|(p, s)| format!("{}+{}", hexs(p), hexs(s)))
                .unwrap_or_else(|| "none".into()),
            self.size,
            segs.join(", ")
        )
    }
}

pub fn hexs(v: u64) -> String {
    format!("{v:#x}")
}

/// Test/diagnostic helpers: little-endian field readers of the payload.
pub fn u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

pub fn u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

/// (vmaddr, vmsize, fileoff, filesize) of payload segment `i`.
pub fn seg_at(payload: &[u8], i: usize) -> (u64, u64, u64, u64) {
    let b = SEG_TABLE + 32 * i;
    (u64(payload, b), u64(payload, b + 8), u64(payload, b + 16), u64(payload, b + 24))
}

/// Pack an xnu kernel Mach-O + ADT (+ optional ramdisk) into the payload the
/// sweet shim boots (and `qemu::run` places directly).
pub fn build(
    kernel: &[u8],
    adt: &[u8],
    cmdline: &str,
    ramdisk: Option<&[u8]>,
) -> Result<(Vec<u8>, BuildInfo)> {
    let mut buf = kernel.to_vec();
    let m = macho::parse(&buf)?;
    let nfix = macho::apply_chained_fixups(&mut buf, &m)?;
    let segs: Vec<macho::Segment> = m.segments.iter().filter(|s| s.vmsize != 0).cloned().collect();
    let text = &segs[0];
    if text.name != "__TEXT" || text.fileoff != 0 {
        bail!("first segment is not __TEXT at fileoff 0");
    }
    // virt_base: 32MB (16K-granule L2 block) below the mach header, so
    // start.s's block mapping that starts at the header's block begins
    // exactly at phys_base.
    let virt_base = text.vmaddr & !(0x200_0000 - 1);
    let mut kernel_end = align(
        segs.iter().map(|s| s.vmaddr + s.vmsize).max().unwrap(),
        PAGE,
    );

    // The ADT decides where the kernel's RAM is (mkadt.py --highmem).
    let (phys_base, mem_size) = adt_dram(adt)?;

    let mut pieces: Vec<(u64, u64, Vec<u8>)> = segs
        .iter()
        .map(|s| {
            (s.vmaddr, s.vmsize, buf[s.fileoff as usize..(s.fileoff + s.filesize) as usize].to_vec())
        })
        .collect();
    drop(buf);
    let mut adt_blob = adt.to_vec();
    let mut ramdisk_info = None;
    if let Some(rd) = ramdisk {
        let rd_size = align(rd.len() as u64, PAGE);
        let rd_va = kernel_end;
        kernel_end = rd_va + rd_size;
        let rd_pa = rd_va - virt_base + phys_base;
        ramdisk_info = Some((rd_pa, rd_size));
        add_ramdisk_to_adt(&mut adt_blob, rd_pa, rd_size)?;
        pieces.push((rd_va, rd_size, rd.to_vec()));
    }

    let nsegs = pieces.len();
    let mut out = vec![0u8; SEG_TABLE + 32 * nsegs];
    let mut seg_table = Vec::with_capacity(nsegs);
    for (vmaddr, vmsize, data) in pieces {
        let off = out.len();
        out.extend_from_slice(&data);
        out.extend(std::iter::repeat(0).take(out.len().wrapping_neg() & 0xf));
        seg_table.push((vmaddr, vmsize, off as u64, data.len() as u64));
    }
    let adt_off = out.len();
    out.extend_from_slice(&adt_blob);
    out.extend(std::iter::repeat(0).take(out.len().wrapping_neg() & 0xf));

    out[0..8].copy_from_slice(MAGIC);
    put_u32le(&mut out, 8, VERSION);
    put_u32le(&mut out, 12, nsegs as u32);
    let fields = [
        virt_base,
        m.entry,
        adt_off as u64,
        adt_blob.len() as u64,
        kernel_end,
        phys_base,
        mem_size,
    ];
    for (i, f) in fields.iter().enumerate() {
        put_u64le(&mut out, 0x10 + 8 * i, *f);
    }
    let cl = cmdline.as_bytes();
    if cl.len() >= BOOT_LINE_LENGTH {
        bail!("cmdline longer than {BOOT_LINE_LENGTH} bytes");
    }
    out[CMDLINE..CMDLINE + cl.len()].copy_from_slice(cl);
    for (i, t) in seg_table.iter().enumerate() {
        put_u64le(&mut out, SEG_TABLE + 32 * i, t.0);
        put_u64le(&mut out, SEG_TABLE + 32 * i + 8, t.1);
        put_u64le(&mut out, SEG_TABLE + 32 * i + 16, t.2);
        put_u64le(&mut out, SEG_TABLE + 32 * i + 24, t.3);
    }
    let size = out.len();
    Ok((
        out,
        BuildInfo {
            cputype: m.cputype,
            cpusub: m.cpusub,
            fixups: nfix,
            virt_base,
            entry: m.entry,
            kernel_end,
            ramdisk: ramdisk_info,
            size,
            segments: segs.iter().map(|s| (s.name.clone(), s.vmaddr, s.vmsize)).collect(),
        },
    ))
}

#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub phys_base: u64,
    pub image: Vec<u8>,
    pub boot_args_pa: u64,
    pub entry_pa: u64,
    pub mem_size: u64,
}

/// Parse a SWEETXNU payload and lay it out at physical addresses — port of
/// `qemu-sweet.py build_image()`.
pub fn build_image(payload: &[u8]) -> Result<PlacedImage> {
    if payload.len() < SEG_TABLE || &payload[0..8] != MAGIC || u32le(payload, 8) != VERSION {
        bail!("not a SWEETXNU v1 payload");
    }
    let nsegs = u32le(payload, 12) as usize;
    let virt_base = u64le(payload, 0x10);
    let entry_va = u64le(payload, 0x18);
    let adt_off = u64le(payload, 0x20) as usize;
    let adt_size = u64le(payload, 0x28) as usize;
    let kernel_end = u64le(payload, 0x30);
    let phys_base = u64le(payload, 0x38);
    let mem_size = u64le(payload, 0x40);
    let cmdline = &payload[CMDLINE..CMDLINE + BOOT_LINE_LENGTH];

    // Python's delta = phys_base - virt_base is negative; do the same math
    // with relative offsets (no u64 wrap): every VA >= virt_base.
    let off_end = kernel_end - virt_base; // bytes of kernel above phys_base
    let adt_pa = phys_base + off_end;
    let ba_pa = align(adt_pa + adt_size as u64, PAGE);
    let top = ba_pa + PAGE;
    let mut img = vec![0u8; (top - phys_base) as usize];
    for i in 0..nsegs {
        let (vmaddr, _vmsize, foff, fsize) = (
            u64le(payload, SEG_TABLE + 32 * i),
            u64le(payload, SEG_TABLE + 32 * i + 8),
            u64le(payload, SEG_TABLE + 32 * i + 16),
            u64le(payload, SEG_TABLE + 32 * i + 24),
        );
        let o = (vmaddr - virt_base) as usize;
        if o + fsize as usize > img.len() || foff as usize + fsize as usize > payload.len() {
            bail!("segment {i} out of range when placing");
        }
        img[o..o + fsize as usize]
            .copy_from_slice(&payload[foff as usize..(foff + fsize) as usize]);
    }
    let ao = (adt_pa - phys_base) as usize;
    if ao + adt_size > img.len() || adt_off + adt_size > payload.len() {
        bail!("ADT out of range when placing");
    }
    img[ao..ao + adt_size].copy_from_slice(&payload[adt_off..adt_off + adt_size]);

    // boot_args: revision 2, version 2, video 1080x2400 depth 32 @ 0x9c000000.
    let mut ba = Vec::with_capacity(736);
    ba.extend_from_slice(&2u16.to_le_bytes()); // revision
    ba.extend_from_slice(&2u16.to_le_bytes()); // version
    ba.extend_from_slice(&[0u8; 4]);
    for v in [virt_base, phys_base, mem_size, top, FB_BASE, 1, FB_STRIDE as u64, FB_W as u64, FB_H as u64, 32] {
        ba.extend_from_slice(&v.to_le_bytes());
    }
    ba.extend_from_slice(&0u32.to_le_bytes());
    ba.extend_from_slice(&[0u8; 4]);
    ba.extend_from_slice(&kernel_end.to_le_bytes()); // adt virtual base
    ba.extend_from_slice(&(adt_size as u32).to_le_bytes());
    ba.extend_from_slice(cmdline);
    ba.extend_from_slice(&[0u8; 4]);
    ba.extend_from_slice(&0u64.to_le_bytes()); // boot_flags
    ba.extend_from_slice(&mem_size.to_le_bytes());
    if ba.len() != 736 {
        bail!("internal: boot_args is {} bytes, expected 736", ba.len());
    }
    img[(ba_pa - phys_base) as usize..(ba_pa - phys_base) as usize + 736]
        .copy_from_slice(&ba);
    Ok(PlacedImage {
        phys_base,
        image: img,
        boot_args_pa: ba_pa,
        entry_pa: phys_base + (entry_va - virt_base),
        mem_size,
    })
}

/// 6-instruction CPU start stub: ldr x0, =ba; ldr x1, =entry; br x1; nop;
/// .quad ba; .quad entry — port of `qemu-sweet.py stub()`.
pub fn stub(ba_pa: u64, entry_pa: u64) -> [u8; 40] {
    let mut s = [0u8; 40];
    for (i, v) in [0x5800_0080u32, 0x5800_00a1, 0xd61f_0020, 0xd503_201f].iter().enumerate() {
        put_u32le(&mut s, i * 4, *v);
    }
    put_u64le(&mut s, 16, ba_pa);
    put_u64le(&mut s, 24, entry_pa);
    s
}

/// Read initialized segment bytes by VA (the payload has no slide). Port of
/// `qemu-sweet.py payload_data()` (used for the `_iso_font` glyph table).
pub fn payload_data(payload: &[u8], va: u64, size: usize) -> Result<Vec<u8>> {
    let nsegs = u32le(payload, 12) as usize;
    for i in 0..nsegs {
        let (vmaddr, _vs, foff, fsize) = (
            u64le(payload, SEG_TABLE + 32 * i),
            u64le(payload, SEG_TABLE + 32 * i + 8),
            u64le(payload, SEG_TABLE + 32 * i + 16),
            u64le(payload, SEG_TABLE + 32 * i + 24),
        );
        if vmaddr <= va && va + size as u64 <= vmaddr + fsize {
            let offset = (foff + va - vmaddr) as usize;
            return Ok(payload[offset..offset + size].to_vec());
        }
    }
    bail!("VA {va:#x}+{size:#x} not in initialized payload segments")
}
