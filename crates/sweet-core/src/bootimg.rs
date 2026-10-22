//! Android boot image v2 writer — the Rust replacement for the `mkbootimg`
//! invocation in `shim/build.sh` (same field values, same page alignment).

use crate::{bail, Result};

pub const PAGE: u32 = 4096;

pub struct BootImageOptions<'a> {
    pub kernel: &'a [u8],
    pub ramdisk: &'a [u8],
    pub dtb: Option<&'a [u8]>,
    pub cmdline: &'a str,
    pub kernel_offset: u32,
    pub ramdisk_offset: u32,
    pub second_offset: u32,
    pub tags_offset: u32,
    pub dtb_offset: u64,
    pub os_version: (u32, u32, u32),
    pub os_patch_level: (u32, u32), // (year-2000 handled by caller), month
    pub page_size: u32,
    pub name: &'a str,
}

impl Default for BootImageOptions<'_> {
    fn default() -> Self {
        // Mirrors shim/build.sh's mkbootimg line.
        BootImageOptions {
            kernel: &[],
            ramdisk: &[],
            dtb: None,
            cmdline: "console=null",
            kernel_offset: 0x8000,
            ramdisk_offset: 0x1000000,
            second_offset: 0,
            tags_offset: 0x100,
            dtb_offset: 0x1f00000,
            os_version: (17, 0, 0),
            os_patch_level: (35, 6), // 2035-06
            page_size: PAGE,
            name: "",
        }
    }
}

fn encode_os_version(major: u32, minor: u32, patch: u32) -> Result<u32> {
    if major > 127 || minor > 63 || patch > 63 {
        bail!("os_version out of range");
    }
    Ok((major << 14) | (minor << 7) | patch)
}

fn encode_patch_level(year: u32, month: u32) -> Result<u32> {
    if year > 99 || month == 0 || month > 12 {
        bail!("os_patch_level out of range (year = year - 2000)");
    }
    Ok((year << 4) | month)
}

/// Build a header_version 2 boot image (1660-byte header).
pub fn build_v2(o: &BootImageOptions) -> Result<Vec<u8>> {
    let page = o.page_size as usize;
    let mut hdr = vec![0u8; 1660];
    hdr[0..8].copy_from_slice(b"ANDROID!");
    let put32 = |h: &mut [u8], off: usize, v: u32| h[off..off + 4].copy_from_slice(&v.to_le_bytes());

    // Page 0 is the header; kernel follows.
    put32(&mut hdr, 8, o.kernel.len() as u32); // kernel_size
    put32(&mut hdr, 12, o.kernel_offset); // kernel_addr
    put32(&mut hdr, 16, o.ramdisk.len() as u32); // ramdisk_size
    put32(&mut hdr, 20, o.ramdisk_offset); // ramdisk_addr
    put32(&mut hdr, 24, 0); // second_size
    put32(&mut hdr, 28, o.second_offset); // second_addr
    put32(&mut hdr, 32, o.tags_offset); // tags_addr
    put32(&mut hdr, 36, o.page_size); // page_size
    put32(&mut hdr, 40, 2); // header_version (v2; field doubles as dt_size in v0)
    let osv = encode_os_version(o.os_version.0, o.os_version.1, o.os_version.2)?;
    let pl = encode_patch_level(o.os_patch_level.0, o.os_patch_level.1)?;
    put32(&mut hdr, 44, osv | (pl << 16)); // os_version | patch_level << 16
    let name = o.name.as_bytes();
    hdr[48..48 + name.len().min(16)].copy_from_slice(&name[..name.len().min(16)]);
    let cmdline = o.cmdline.as_bytes();
    if cmdline.len() > 512 {
        bail!("cmdline exceeds 512 bytes");
    }
    hdr[64..64 + cmdline.len()].copy_from_slice(cmdline);
    // id[8] @ 576 left zero; extra_cmdline @ 608 left zero.
    put32(&mut hdr, 1632, 0); // recovery_dtbo_size
    // recovery_dtbo_offset u64 @1636 zero
    put32(&mut hdr, 1644, 1660); // header_size
    let dtb_len = o.dtb.map(|d| d.len()).unwrap_or(0);
    put32(&mut hdr, 1648, dtb_len as u32); // dtb_size
    hdr[1652..1660].copy_from_slice(&o.dtb_offset.to_le_bytes()); // dtb_addr u64

    let mut out = vec![0u8; page];
    out[..hdr.len()].copy_from_slice(&hdr);
    let mut push_page_aligned = |out: &mut Vec<u8>, data: &[u8]| {
        let start = out.len();
        out.extend_from_slice(data);
        out.extend(std::iter::repeat(0).take((page - out.len() % page) % page));
        let _ = start;
    };
    push_page_aligned(&mut out, o.kernel);
    push_page_aligned(&mut out, o.ramdisk);
    if let Some(dtb) = o.dtb {
        push_page_aligned(&mut out, dtb);
    }
    Ok(out)
}
