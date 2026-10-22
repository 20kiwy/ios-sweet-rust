//! Apple-logo framebuffer composition (the IPSW asset injection at
//! 0x9c000000): black BGRA canvas + centered logo, plus the QEMU loader blob
//! writer and the framebuffer non-blank/visible-logo analysis used by
//! verdicts.

use crate::png::Image;
use crate::{bail, FB_H, FB_STRIDE, FB_W, Result};

/// Black BGRA canvas (a8r8g8b8 little endian = B G R A in memory).
pub fn blank_canvas() -> Vec<u8> {
    vec![0u8; FB_STRIDE as usize * FB_H as usize]
}

/// Nearest-neighbour resize (RGBA).
pub fn resize(img: &Image, w: u32, h: u32) -> Image {
    let mut out = Image::new(w, h);
    for y in 0..h {
        let sy = (y as u64 * img.height as u64 / h as u64) as u32;
        for x in 0..w {
            let sx = (x as u64 * img.width as u64 / w as u64) as u32;
            out.rgba[(y as usize * w as usize + x as usize) * 4..][..4]
                .copy_from_slice(&img.pixel(sx, sy));
        }
    }
    out
}

/// Compose `logo` centered on a black canvas, scaled so it spans
/// `scale_pct` of the narrow dimension. Returns the BGRA framebuffer.
pub fn compose(logo: &Image, scale_pct: u32) -> Vec<u8> {
    let mut fb = blank_canvas();
    let target = (FB_W.min(FB_H) as u64 * scale_pct as u64 / 100).min(u32::MAX as u64) as u32;
    let (w, h) = if logo.width >= logo.height {
        let w = target;
        (w, (logo.height as u64 * w as u64 / logo.width as u64) as u32)
    } else {
        let h = target;
        ((logo.width as u64 * h as u64 / logo.height as u64) as u32, h)
    };
    let scaled = resize(logo, w.max(1), h.max(1));
    let ox = (FB_W - w) / 2;
    let oy = (FB_H - h) / 2;
    for y in 0..h {
        for x in 0..w {
            let [r, g, b, a] = scaled.pixel(x, y);
            if a == 0 {
                continue;
            }
            let o = ((oy + y) as usize) * FB_STRIDE as usize + ((ox + x) as usize) * 4;
            // alpha blend over black (assets are anti-aliased grey/white)
            fb[o] = (b as u16 * a as u16 / 255) as u8;
            fb[o + 1] = (g as u16 * a as u16 / 255) as u8;
            fb[o + 2] = (r as u16 * a as u16 / 255) as u8;
            fb[o + 3] = 255;
        }
    }
    fb
}

/// Write the QEMU loader blob for `-device loader,file=...,addr=0x9c000000`.
pub fn loader_blob(fb: &[u8]) -> Result<Vec<u8>> {
    if fb.len() != FB_STRIDE as usize * FB_H as usize {
        bail!("framebuffer is {} bytes, expected {}", fb.len(), FB_STRIDE as usize * FB_H as usize);
    }
    Ok(fb.to_vec())
}

/// Fraction (0..1) of pixels that are not pure black.
pub fn nonblack_fraction(fb: &[u8]) -> f64 {
    let stride = FB_STRIDE as usize;
    let mut nonblack = 0u64;
    let mut total = 0u64;
    for y in 0..FB_H as usize {
        for x in 0..FB_W as usize {
            let o = y * stride + x * 4;
            total += 1;
            if fb[o] != 0 || fb[o + 1] != 0 || fb[o + 2] != 0 {
                nonblack += 1;
            }
        }
    }
    nonblack as f64 / total as f64
}

/// Fraction of pixels within the center box that are bright (>= `thresh` on
/// any channel) — the logo-visibility check for verdicts.
pub fn bright_fraction(fb: &[u8], thresh: u8) -> f64 {
    let stride = FB_STRIDE as usize;
    let (x0, x1) = (FB_W / 4, FB_W * 3 / 4);
    let (y0, y1) = (FB_H / 3, FB_H * 2 / 3);
    let mut bright = 0u64;
    let mut total = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let o = y as usize * stride + x as usize * 4;
            total += 1;
            if fb[o] >= thresh || fb[o + 1] >= thresh || fb[o + 2] >= thresh {
                bright += 1;
            }
        }
    }
    bright as f64 / total as f64
}

/// True when the framebuffer shows the hello welcome screen: solid-white
/// text (>= 250) in the center box over an almost-black background. The
/// dithered Apple logo never satisfies both (its pixels are sparse grey and
/// it keeps ~17% non-black coverage), so this cleanly separates the two
/// boot milestones without relying on serial output.
pub fn is_hello_screen(fb: &[u8]) -> bool {
    nonblack_fraction(fb) < 0.05 && bright_fraction(fb, 250) > 0.01
}
