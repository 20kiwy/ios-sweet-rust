//! The iOS welcome-screen milestone: render the iconic first frame of the
//! iPhone setup experience — a white "Hello" centered on black (plus the home
//! indicator) — as a BGRA framebuffer the demo kernel copies to 0x9c000000.
//!
//! Glyphs are hand-defined 8x12 bitmaps (bit 7 = leftmost pixel), scaled by
//! an integer factor so the word fills the 1080x2400 canvas the way iOS
//! draws it. All rendering happens host-side in Rust; the guest kernel only
//! runs a memcpy, keeping the hand-assembled payload trivial to trust.

use crate::fblogo::blank_canvas;
use crate::{FB_H, FB_W};

/// One glyph: 12 rows, MSB-left, rows top-to-bottom.
type Glyph = [u8; 12];

const H: Glyph = [
    0xc3, 0xc3, 0xc3, 0xc3, 0xff, 0xff, 0xc3, 0xc3, 0xc3, 0xc3, 0xc3, 0xc3,
];
/// lowercase e, x-height rows 4..12 (8 rows used)
const E: Glyph = [
    0, 0, 0, 0, 0x3c, 0x42, 0x42, 0x7e, 0x82, 0x82, 0x7c, 0,
];
/// lowercase l: full-height bar with a top-left flag
const L: Glyph = [
    0x18, 0x1c, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18,
];
/// lowercase o, x-height rows 4..12
const O: Glyph = [
    0, 0, 0, 0, 0x3c, 0x42, 0x81, 0x81, 0x81, 0x81, 0x42, 0x3c,
];

fn glyph_for(c: u8) -> Glyph {
    match c {
        b'H' => H,
        b'e' => E,
        b'l' => L,
        b'o' => O,
        _ => [0; 12],
    }
}

/// Draw `text` centered horizontally, vertically centered as a block, each
/// glyph pixel expanded to `scale x scale`. Returns the widest row height.
fn draw_text(fb: &mut [u8], text: &str, scale: u32, y_top: u32, color: [u8; 3]) {
    let glyph_w = 8 * scale;
    let gap = scale;
    let word_w = text.len() as u32 * glyph_w + (text.len() as u32 - 1) * gap;
    let mut x0 = (FB_W.saturating_sub(word_w)) / 2;
    let stride = (FB_W * 4) as usize;
    for &c in text.as_bytes() {
        let g = glyph_for(c);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..8u32 {
                if bits & (0x80 >> col) == 0 {
                    continue;
                }
                let px = x0 + col * scale;
                let py = y_top + row as u32 * scale;
                for dy in 0..scale {
                    let y = py + dy;
                    if y >= FB_H {
                        continue;
                    }
                    for dx in 0..scale {
                        let x = px + dx;
                        if x >= FB_W {
                            continue;
                        }
                        let o = y as usize * stride + x as usize * 4;
                        fb[o] = color[2];
                        fb[o + 1] = color[1];
                        fb[o + 2] = color[0];
                        fb[o + 3] = 255;
                    }
                }
            }
        }
        x0 += glyph_w + gap;
    }
}

/// Render the welcome screen: black canvas, white "Hello" at `scale` (18
/// gives a 792px word on 1080 — close to how iOS fills the display), and the
/// iOS home indicator near the bottom. Returns the BGRA framebuffer.
pub fn welcome_fb(scale: u32) -> Vec<u8> {
    let mut fb = blank_canvas();
    // opaque canvas (alpha byte = 255 on every pixel)
    for a in fb.iter_mut().skip(3).step_by(4) {
        *a = 255;
    }
    // H is 12 rows tall: 12*scale px block, vertically centered.
    let y_top = (FB_H - 12 * scale) / 2;
    draw_text(&mut fb, "Hello", scale, y_top, [255, 255, 255]);
    // home indicator: 1/3-width rounded-ish bar, ~25% white, 48px above bottom
    let bar_w = FB_W / 3;
    let bar_h = 10u32.max(scale / 2);
    let bar_y = FB_H - 48 - bar_h;
    let x0 = (FB_W - bar_w) / 2;
    let stride = (FB_W * 4) as usize;
    for dy in 0..bar_h {
        for dx in 0..bar_w {
            let o = (bar_y + dy) as usize * stride + (x0 + dx) as usize * 4;
            fb[o] = 102;
            fb[o + 1] = 102;
            fb[o + 2] = 102;
            fb[o + 3] = 255;
        }
    }
    fb
}

/// Serial marker printed by the hello kernel (asserted by verdicts/CI).
pub const SERIAL_MARKER: &str = "SWEET-RUST hello screen drawn";
