//! SWEET boot pipeline, in Rust.
//!
//! A 1:1 reimplementation of the `ios-sweet` Python boot toolchain:
//!
//! | Module | Python original | What it does |
//! | --- | --- | --- |
//! | [`macho`] | `tools/mkpayload.py` (parse/fixups) | Mach-O parse + LC_DYLD_CHAINED_FIXUPS rebases |
//! | [`adt`] | `tools/adt.py` | Apple Device Tree parse / serialize / dump |
//! | [`mkadt`] | `tools/mkadt.py` | SWEET/QEMU ADT generation (iPhone X identity) |
//! | [`payload`] | `tools/mkpayload.py` + `tools/qemu-sweet.py` | SWEETXNU payload, RAM image, boot_args, CPU stub |
//! | [`im4p`] | `tools/adt.py` (DER part) | IM4P/IMG4 unwrap, LZFSE (`bvx2`) decompression |
//! | [`ipsw`] | `tools/remotezip.py` | IPSW zip listing/extract, local file or HTTP range (curl) |
//! | [`png`] | `qemu-sweet.py write_png` + PNG decode | PNG encode/decode over vendored miniz |
//! | [`ibootim`] | all_flash asset format | iBootIm container + SSZL (iBoot LZSS) decode |
//! | [`fblogo`] | logo asset injection at `0x9c000000` | Apple logo → BGRA framebuffer canvas |
//! | [`qemu`] | `tools/qemu-sweet.py` | QEMU virt boot, QMP (unix+TCP), screenshots, verdicts |
//! | [`bootimg`] | `shim/build.sh` (mkbootimg) | Android boot image v2 writer |
//! | [`demo`] | new | synthetic arm64 demo kernel for QEMU/CI logo tests |
//!
//! The native compression code is vendored C (Apple LZFSE, miniz) — see
//! `src/vendor/PROVENANCE.md`.

pub mod adt;
pub mod apfs;
pub mod bootimg;
pub mod cpio;
pub mod control;
pub mod demo;
pub mod dmg;
pub mod fblogo;
pub mod hash;
pub mod hello;
pub mod hfs;
pub mod ibootim;
pub mod im4p;
pub mod ipsw;
pub mod kexts;
pub mod macho;
pub mod mkadt;
pub mod png;
pub mod payload;
pub mod plist;
pub mod qemu;

mod error;
pub use error::{Error, Result};

/// Framebuffer the ABL leaves (and QEMU loader fills): 1080x2400 BGRA.
pub const FB_BASE: u64 = 0x9c000000;
pub const FB_W: u32 = 1080;
pub const FB_H: u32 = 2400;
pub const FB_STRIDE: u32 = FB_W * 4;
pub const PAGE: u64 = 0x4000;

/// arm64 helpers used across modules.
pub mod aarch64 {
    /// Little-endian u32 at `off`.
    pub fn u32le(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }
    pub fn u64le(b: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }
    pub fn put_u32le(b: &mut [u8], off: usize, v: u32) {
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    pub fn put_u64le(b: &mut [u8], off: usize, v: u64) {
        b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }
    pub fn align(v: u64, a: u64) -> u64 {
        (v + a - 1) & !(a - 1)
    }
}

#[cfg(test)]
mod tests;
