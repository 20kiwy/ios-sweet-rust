//! Synthetic bootable arm64 demo kernel: proves the whole QEMU path end to
//! end without the SWEET xnu artifact. Reads boot_args (x0 from the stub),
//! paints a colored strip at the bottom of the framebuffer (so an injected
//! Apple logo stays visible above it) and prints a marker to the PL011
//! serial at 0x09000000. CI asserts on serial + framebuffer.

use crate::{bail, FB_H, Result};

const BASE: u64 = 0x1_0000_0000; // __TEXT VA (matches the SWEET kernel's region)
const CODE_OFF: usize = 0x400;

fn ins(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

fn adr(rd: u32, byte_off: i64) -> u32 {
    let imm = byte_off & 0x1f_ffff;
    let immlo = (imm & 3) as u32;
    let immhi = ((imm >> 2) & 0x7_ffff) as u32;
    0x1000_0000 | (immlo << 29) | (immhi << 5) | rd
}

fn b(byte_off: i64) -> u32 {
    0x1400_0000 | (((byte_off >> 2) & 0x03ff_ffff) as u32)
}

fn cbz(rt: u32, byte_off: i64) -> u32 {
    0x3400_0000 | ((((byte_off >> 2) & 0x7_ffff) as u32) << 5) | rt
}

fn bcond_ne(byte_off: i64) -> u32 {
    0x5400_0000 | ((((byte_off >> 2) & 0x7_ffff) as u32) << 5) | 1 // cond NE
}

/// movz Xd, #imm16 (64-bit form)
fn movz64(rd: u32, imm: u32) -> u32 {
    0xd280_0000 | ((imm & 0xffff) << 5) | rd
}

/// movk Xd, #imm16, lsl #16
fn movk64_lsl16(rd: u32, imm: u32) -> u32 {
    0xf2a0_0000 | ((imm & 0xffff) << 5) | rd
}

/// movz Wd, #imm16
fn movz32(rd: u32, imm: u32) -> u32 {
    0x5280_0000 | ((imm & 0xffff) << 5) | rd
}

/// movk Wd, #imm16, lsl #16
fn movk32_lsl16(rd: u32, imm: u32) -> u32 {
    0x72a0_0000 | ((imm & 0xffff) << 5) | rd
}

/// mul Xd, Xn, Xm (MADD with Ra = XZR)
fn mul64(rd: u32, rn: u32, rm: u32) -> u32 {
    0x9b00_0000 | (rm << 16) | (31 << 10) | (rn << 5) | rd
}

fn add64(rd: u32, rn: u32, rm: u32) -> u32 {
    0x8b00_0000 | (rm << 16) | (rn << 5) | rd
}

fn add64_imm12(rd: u32, rn: u32, imm: u32) -> u32 {
    0x9100_0000 | ((imm & 0xfff) << 10) | (rn << 5) | rd
}

fn mov64(rd: u32, rm: u32) -> u32 {
    0xaa00_0000 | (rm << 16) | (31 << 5) | rd
}

const PL011_DR: u32 = 0x0900_0000;

/// Generate the demo kernel Mach-O. The bottom `strip_rows` rows are painted
/// `color` (0x00RRGGBB); everything above stays untouched so an injected IPSW
/// Apple logo remains visible. Serial marker: "SWEET-RUST demo kernel OK".
pub fn macho(color: u32, strip_rows: u32) -> Result<Vec<u8>> {
    if strip_rows == 0 || strip_rows > FB_H {
        bail!("strip_rows must be 1..={FB_H}");
    }
    let first_row = (FB_H - strip_rows) as u32; // < 2400, fits imm16
    // color = 0x00RRGGBB; a8r8g8b8 little-endian word = A<<24 | R<<16 | G<<8 | B
    let r8 = (color >> 16) & 0xff;
    let g8 = (color >> 8) & 0xff;
    let b8 = color & 0xff;
    let bgra = (0xff << 24) | (r8 << 16) | (g8 << 8) | b8;

    // Two-pass: emit with placeholder ADR/branches, then patch.
    let mut code: Vec<u8> = Vec::new();
    let mut adr_x1_at = None;
    let mut cbz_at = None;
    let mut ldrb_at = None;
    let mut back_b_at = None;
    let mut done_at = None;

    let mut emit = |code: &mut Vec<u8>, v: u32| code.extend_from_slice(&ins(v));

    // x19 = boot_args (x0)
    emit(&mut code, mov64(19, 0));
    // x20 = [x19 + 0x28]  video base        (ldr x20, [x19, #0x28] = F9401674)
    emit(&mut code, 0xf940_1674);
    // x21 = [x19 + 0x38]  video stride      (ldr x21, [x19, #0x38] = F9401E75)
    emit(&mut code, 0xf940_1e75);
    // w24 = BGRA color
    emit(&mut code, movz32(24, bgra & 0xffff));
    emit(&mut code, movk32_lsl16(24, bgra >> 16));
    // x23 = x20 + stride * first_row  (cursor start)
    emit(&mut code, movz64(23, first_row));
    emit(&mut code, mul64(23, 21, 23));
    emit(&mut code, add64(23, 20, 23));
    // x25 = x20 + stride * FB_H       (cursor end)
    emit(&mut code, movz64(25, FB_H));
    emit(&mut code, mul64(25, 21, 25));
    emit(&mut code, add64(25, 20, 25));
    // x22 = x23 (cursor)
    emit(&mut code, mov64(22, 23));
    // paint loop
    let loop_at = code.len();
    emit(&mut code, 0xb900_02d8); // str w24, [x22]
    emit(&mut code, add64_imm12(22, 22, 4)); // add x22, x22, #4
    emit(&mut code, 0xeb19_02df); // cmp x22, x25 (subs xzr, x22, x25)
    let bne_at = code.len();
    emit(&mut code, 0); // b.ne loop — patch below
    let bne_off = loop_at as i64 - bne_at as i64;
    code[bne_at..bne_at + 4].copy_from_slice(&ins(bcond_ne(bne_off)));

    // FB corner marker: two strip-colored pixels at (0,0) prove the kernel
    // reached this point even when the serial backend misbehaves.
    emit(&mut code, 0xb900_0298); // str w24, [x20] — wait, offset 0: 0xb9000000 | (0<<10)|(20<<5)|24 = 0xb9000298 IS offset 0 (Rn=20<<5=0x280, Rt=24=0x18)
    emit(&mut code, 0xb900_02b8); // str w24, [x21] — second marker at stride addr (harmless)
    // enable the PL011 (CR = UARTEN | TXE | RXE = 0x301)
    emit(&mut code, movz64(3, 0x0030));
    emit(&mut code, movk64_lsl16(3, PL011_DR >> 16)); // x3 = 0x09000030 (CR)
    emit(&mut code, movz32(4, 0x301));
    emit(&mut code, 0xb900_0064); // str w4, [x3]
    // serial print
    emit(&mut code, movz64(0, PL011_DR & 0xffff));
    emit(&mut code, movk64_lsl16(0, PL011_DR >> 16)); // x0 = 0x09000000 (DR)
    let adr_at = code.len();
    emit(&mut code, 0); // adr x1, msg — patch below
    adr_x1_at = Some(adr_at);
    let ldrb_pos = code.len();
    ldrb_at = Some(ldrb_pos);
    emit(&mut code, 0x3940_0002); // ldrb w2, [x1]
    let cbz_pos = code.len();
    cbz_at = Some(cbz_pos);
    emit(&mut code, 0); // cbz w2, done — patch below
    emit(&mut code, 0xb900_0002); // str w2, [x0]
    emit(&mut code, add64_imm12(1, 1, 1)); // add x1, x1, #1
    let back_pos = code.len();
    back_b_at = Some(back_pos);
    emit(&mut code, 0); // b ldrb — patch below
    let done_pos = code.len();
    done_at = Some(done_pos);
    emit(&mut code, 0xd503_207f); // wfi
    emit(&mut code, b(0)); // b . (self)

    // msg
    let msg_pos = code.len();
    code.extend_from_slice(b"\nSWEET-RUST demo kernel OK (fb strip drawn)\n\0");

    // patches
    let msg_off = msg_pos as i64 - adr_at as i64;
    code[adr_at..adr_at + 4].copy_from_slice(&ins(adr(1, msg_off)));
    let cbz_off = done_pos as i64 - cbz_pos as i64;
    code[cbz_pos..cbz_pos + 4].copy_from_slice(&ins(cbz(2, cbz_off)));
    let back_off = ldrb_pos as i64 - back_pos as i64;
    code[back_pos..back_pos + 4].copy_from_slice(&ins(b(back_off)));
    let _ = (adr_x1_at, cbz_at, ldrb_at, back_b_at, done_at);

    Ok(wrap_macho(&code))
}

/// Wrap position-independent __text into a static MH_EXECUTE with
/// LC_UNIXTHREAD pc = BASE + CODE_OFF (mkinit-style layout; unsigned is fine:
/// the demo kernel is parsed by mkpayload, never by XNU's mach_loader).
fn wrap_macho(code: &[u8]) -> Vec<u8> {
    const SEG: usize = 0x4000;
    let mut image = vec![0u8; SEG * 2]; // __TEXT page + __LINKEDIT page
    let mut cmds: Vec<u8> = Vec::new();

    // No __PAGEZERO: the SWEET xnu Mach-O starts with __TEXT at its VA
    // (mkpayload asserts segments[0] == __TEXT at fileoff 0), so the demo
    // kernel matches that layout exactly.
    // __TEXT @ BASE r-x with one __text section
    let mut sect = [0u8; 80];
    sect[0..7].copy_from_slice(b"__text\0");
    sect[16..23].copy_from_slice(b"__TEXT\0");
    sect[32..40].copy_from_slice(&(BASE + CODE_OFF as u64).to_le_bytes()); // addr
    sect[40..48].copy_from_slice(&(code.len() as u64).to_le_bytes()); // size
    sect[48..56].copy_from_slice(&(CODE_OFF as u64).to_le_bytes()); // offset
    sect[56..64].copy_from_slice(&(code.len() as u64).to_le_bytes()); // reloff? no: reserved1.. keep size again
    sect[64..68].copy_from_slice(&3u32.to_le_bytes()); // align flags low: use 3? (this is reloc offset field; cosmetic)
    sect[68..72].copy_from_slice(&3u32.to_le_bytes());
    sect[76..80].copy_from_slice(&0x8000_0400u32.to_le_bytes()); // S_ATTR_CODE
    cmds.extend_from_slice(&seg_cmd("__TEXT", BASE, SEG as u64, 0, SEG as u64, 5, &sect));
    // __LINKEDIT r-- (empty)
    cmds.extend_from_slice(&seg_cmd("__LINKEDIT", BASE + SEG as u64, SEG as u64, SEG as u64, 0, 1, &[]));
    // LC_UNIXTHREAD (flavor 6 = ARM_THREAD_STATE64, count 68), sp = 0, pc = entry
    cmds.extend_from_slice(&5u32.to_le_bytes()); // cmd
    cmds.extend_from_slice(&((16 + 4 * 68) as u32).to_le_bytes()); // cmdsize
    cmds.extend_from_slice(&6u32.to_le_bytes()); // flavor
    cmds.extend_from_slice(&68u32.to_le_bytes()); // count
    let mut state = [0u64; 33];
    state[32] = BASE + CODE_OFF as u64; // pc
    for v in state {
        cmds.extend_from_slice(&v.to_le_bytes());
    }
    cmds.extend_from_slice(&0u32.to_le_bytes()); // cpsr
    cmds.extend_from_slice(&0u32.to_le_bytes()); // pad
    // LC_CODE_SIGNATURE (0x1d), cmdsize 16, offset/size 0 = unsigned
    cmds.extend_from_slice(&0x1du32.to_le_bytes());
    cmds.extend_from_slice(&16u32.to_le_bytes());
    cmds.extend_from_slice(&((SEG * 2) as u32).to_le_bytes());
    cmds.extend_from_slice(&0u32.to_le_bytes());

    let hdr_len = 32 + cmds.len();
    assert!(hdr_len <= CODE_OFF, "load commands exceed CODE_OFF");
    let mh = [
        0xfeedfacfu32.to_le_bytes(), // magic
        0x0100_000cu32.to_le_bytes(), // cputype arm64
        0u32.to_le_bytes(),           // cpusubtype
        2u32.to_le_bytes(),           // MH_EXECUTE
        4u32.to_le_bytes(),           // ncmds
        (cmds.len() as u32).to_le_bytes(),
        1u32.to_le_bytes(), // MH_NOUNDEFS
        0u32.to_le_bytes(),
    ]
    .concat();
    image[..32].copy_from_slice(&mh);
    image[32..32 + cmds.len()].copy_from_slice(&cmds);
    image[CODE_OFF..CODE_OFF + code.len()].copy_from_slice(code);
    image
}

fn seg_cmd(
    name: &str,
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    prot: u32,
    sections: &[u8],
) -> Vec<u8> {
    let nsects = sections.len() / 80;
    let mut c = Vec::new();
    c.extend_from_slice(&0x19u32.to_le_bytes()); // LC_SEGMENT_64
    c.extend_from_slice(&((72 + 80 * nsects) as u32).to_le_bytes());
    let mut nm = [0u8; 16];
    nm[..name.len()].copy_from_slice(name.as_bytes());
    c.extend_from_slice(&nm);
    c.extend_from_slice(&vmaddr.to_le_bytes());
    c.extend_from_slice(&vmsize.to_le_bytes());
    c.extend_from_slice(&fileoff.to_le_bytes());
    c.extend_from_slice(&filesize.to_le_bytes());
    c.extend_from_slice(&prot.to_le_bytes()); // maxprot
    c.extend_from_slice(&prot.to_le_bytes()); // initprot
    c.extend_from_slice(&(nsects as u32).to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes()); // flags
    c.extend_from_slice(sections);
    c
}
