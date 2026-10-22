//! Minimal newc cpio writer — for the empty ramdisk ABL requires
//! (shim/build.sh: "an empty newc cpio (ABL dislikes a zero-size ramdisk)").

use crate::Result;

fn octal(v: u32) -> [u8; 8] {
    let mut b = [b'0'; 8];
    let s = format!("{v:08o}");
    b[..s.len()].copy_from_slice(s.as_bytes());
    b
}

fn entry(out: &mut Vec<u8>, name: &str, mode: u32, size: usize, data: &[u8]) -> Result<()> {
    const MAGIC: &[u8; 6] = b"070701";
    let name_len = name.len() + 1;
    out.extend_from_slice(MAGIC);
    for v in [
        0u32,        // ino
        mode,        // mode
        0,           // uid
        0,           // gid
        1,           // nlink
        0,           // mtime
        size as u32, // filesize
        0,           // devmajor
        0,           // devminor
        0,           // rdevmajor
        0,           // rdevminor
        name_len as u32,
        0, // check
    ] {
        out.extend_from_slice(&octal(v));
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend(std::iter::repeat(0).take((4 - out.len() % 4) % 4));
    out.extend_from_slice(data);
    out.extend(std::iter::repeat(0).take((4 - out.len() % 4) % 4));
    Ok(())
}

/// An empty ("." + TRAILER!!!) newc cpio, uncompressed.
pub fn empty_cpio() -> Result<Vec<u8>> {
    let mut out = Vec::new();
    entry(&mut out, ".", 0o040755, 0, &[])?;
    entry(&mut out, "TRAILER!!!", 0, 0, &[])?;
    out.extend(std::iter::repeat(0).take((512 - out.len() % 512) % 512));
    Ok(out)
}
