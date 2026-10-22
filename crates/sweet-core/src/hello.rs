//! The iOS hello screen — rendered from the REAL vector data Apple ships in
//! the IPSW: `SpringBoard.framework/<lang>.lproj/CursiveHello.json`, whose
//! `strokes` are cubic Bézier chains (p0..p3, each [x, y, width, pressure])
//! that SpringBoard rasterizes for the first-boot welcome animation.
//!
//! This module parses that JSON and rasterizes the final frame (all strokes
//! drawn) onto the 1080x2400 BGRA framebuffer: white cursive "hello" on
//! black, positioned like iOS (centered horizontally, ~40% down). No
//! hand-drawn stand-ins — every glyph comes from the IPSW.
//!
//! The guest kernel only memcpy's the finished frame to 0x9c000000; all
//! rendering happens host-side.

use crate::{bail, FB_H, FB_W, Result};

// ---------------------------------------------------------------- JSON

#[derive(Debug, Clone)]
enum Json {
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
    Null,
    Bool(bool),
}

struct Jp<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Jp<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }
    fn ws(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&self) -> u8 {
        self.b[self.i]
    }
    fn eat(&mut self, c: u8) -> Result<()> {
        self.ws();
        if self.i >= self.b.len() || self.b[self.i] != c {
            bail!("json: expected {c:?} at {}", self.i);
        }
        self.i += 1;
        Ok(())
    }
    fn value(&mut self) -> Result<Json> {
        self.ws();
        match self.peek() {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Json::Str(self.string()?)),
            b't' | b'f' => self.boolean(),
            b'n' => self.null(),
            _ => self.number(),
        }
    }
    fn object(&mut self) -> Result<Json> {
        self.eat(b'{')?;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == b'}' {
            self.i += 1;
            return Ok(Json::Obj(out));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.eat(b':')?;
            let v = self.value()?;
            out.push((k, v));
            self.ws();
            match self.peek() {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    break;
                }
                c => bail!("json: expected , or }} got {c:?}"),
            }
        }
        Ok(Json::Obj(out))
    }
    fn array(&mut self) -> Result<Json> {
        self.eat(b'[')?;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == b']' {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            out.push(self.value()?);
            self.ws();
            match self.peek() {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    break;
                }
                c => bail!("json: expected , or ] got {c:?}"),
            }
        }
        Ok(Json::Arr(out))
    }
    fn string(&mut self) -> Result<String> {
        self.eat(b'"')?;
        let mut s = String::new();
        while self.i < self.b.len() && self.b[self.i] != b'"' {
            let c = self.b[self.i];
            if c == b'\\' {
                self.i += 1;
                let e = self.b[self.i];
                s.push(match e {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'u' => {
                        let hex = std::str::from_utf8(&self.b[self.i + 1..self.i + 5])
                            .map_err(|_| crate::Error::Msg("json: bad \\u".into()))?;
                        let cp = u32::from_str_radix(hex, 16)
                            .map_err(|_| crate::Error::Msg("json: bad \\u".into()))?;
                        self.i += 4;
                        char::from_u32(cp).unwrap_or('\u{fffd}')
                    }
                    other => other as char,
                });
                self.i += 1;
            } else {
                // UTF-8 passthrough: copy the full sequence
                let len = utf8_len(c);
                let end = (self.i + len).min(self.b.len());
                s.push_str(&String::from_utf8_lossy(&self.b[self.i..end]));
                self.i = end;
            }
        }
        self.eat(b'"')?;
        Ok(s)
    }
    fn boolean(&mut self) -> Result<Json> {
        if self.b[self.i..].starts_with(b"true") {
            self.i += 4;
            Ok(Json::Bool(true))
        } else {
            self.i += 5;
            Ok(Json::Bool(false))
        }
    }
    fn null(&mut self) -> Result<Json> {
        self.i += 4;
        Ok(Json::Null)
    }
    fn number(&mut self) -> Result<Json> {
        let start = self.i;
        while self.i < self.b.len()
            && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| crate::Error::Msg("json: bad number".into()))?;
        s.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| crate::Error::Msg(format!("json: bad number {s:?}")))
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// ------------------------------------------------------- cursive strokes

/// One cubic Bézier segment: p0..p3, each [x, y, width, pressure].
#[derive(Debug, Clone, Copy)]
pub struct Seg {
    pub p: [[f32; 4]; 4],
}

/// Parse the CursiveHello.json `strokes` structure.
pub fn parse_strokes(json: &[u8]) -> Result<Vec<Vec<Seg>>> {
    let mut jp = Jp::new(json);
    let root = jp.value()?;
    let arr = match root {
        Json::Obj(o) => o
            .into_iter()
            .find(|(k, _)| k == "strokes")
            .map(|(_, v)| v)
            .ok_or_else(|| crate::Error::Msg("no strokes key".into()))?,
        _ => bail!("unexpected json root"),
    };
    let mut out = Vec::new();
    if let Json::Arr(strokes) = arr {
        for st in strokes {
            let mut segs = Vec::new();
            if let Json::Arr(parts) = st {
                for seg in parts {
                    if let Json::Obj(o) = seg {
                        let mut p = [[0f32; 4]; 4];
                        for (idx, key) in ["p0", "p1", "p2", "p3"].iter().enumerate() {
                            if let Some((_, Json::Arr(a))) =
                                o.iter().find(|(k, _)| k == key)
                            {
                                for (j, v) in a.iter().enumerate().take(4) {
                                    if let Json::Num(n) = v {
                                        p[idx][j] = *n as f32;
                                    }
                                }
                            }
                        }
                        segs.push(Seg { p });
                    }
                }
            }
            if !segs.is_empty() {
                out.push(segs);
            }
        }
    }
    if out.is_empty() {
        bail!("no strokes parsed");
    }
    Ok(out)
}

fn bezier(p0: &[f32; 4], p1: &[f32; 4], p2: &[f32; 4], p3: &[f32; 4], t: f32) -> [f32; 4] {
    let mt = 1.0 - t;
    let mut out = [0f32; 4];
    let w = [
        mt * mt * mt,
        3.0 * mt * mt * t,
        3.0 * mt * t * t,
        t * t * t,
    ];
    for j in 0..4 {
        out[j] = w[0] * p0[j] + w[1] * p1[j] + w[2] * p2[j] + w[3] * p3[j];
    }
    out
}

struct Canvas {
    fb: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        let mut fb = vec![0u8; (FB_W * FB_H * 4) as usize];
        for a in fb.iter_mut().skip(3).step_by(4) {
            *a = 255; // opaque
        }
        Self { fb }
    }
    /// Soft white disc, max-blended (stroke rasterization).
    fn dot(&mut self, x: f32, y: f32, r: f32) {
        if r < 0.25 {
            return;
        }
        let stride = (FB_W * 4) as usize;
        let (xi, yi) = (x as i64, y as i64);
        let ri = (r.ceil() as i64) + 1;
        for dy in -ri..=ri {
            let yy = yi + dy;
            if yy < 0 || yy >= FB_H as i64 {
                continue;
            }
            for dx in -ri..=ri {
                let xx = xi + dx;
                if xx < 0 || xx >= FB_W as i64 {
                    continue;
                }
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                if d <= r {
                    // solid core to 70% radius, then a soft edge
                    let a = if d < 0.7 * r { 1.0 } else { 1.0 - (d / r - 0.7) / 0.3 * 0.9 };
                    let o = yy as usize * stride + xx as usize * 4;
                    let v = (a * 255.0) as u8;
                    self.fb[o] = self.fb[o].max(v);
                    self.fb[o + 1] = self.fb[o + 1].max(v);
                    self.fb[o + 2] = self.fb[o + 2].max(v);
                }
            }
        }
    }
}

/// Rasterize the cursive hello onto the framebuffer: final frame (all
/// strokes drawn), scaled to ~72% of the narrow axis, centered horizontally,
/// at ~40% height — matching the iOS welcome presentation.
pub fn render_strokes(strokes: &[Vec<Seg>]) -> Vec<u8> {
    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for st in strokes {
        for seg in st {
            // include ALL control points: the curve can bow past its endpoints
            for p in seg.p.iter() {
                x0 = x0.min(p[0]);
                x1 = x1.max(p[0]);
                y0 = y0.min(p[1]);
                y1 = y1.max(p[1]);
            }
        }
    }
    let w = (x1 - x0).max(1.0);
    let h = (y1 - y0).max(1.0);
    // iOS spans the word ~92% of the width, word center ~42% down.
    // The stored pen coordinates are MIRRORED relative to the display
    // (verified: ja hiragana renders flipped without this) — flip X.
    let scale = ((FB_W as f32 * 0.92) / w).min((FB_H as f32 * 0.30) / h);
    let ox = FB_W as f32 / 2.0 + (x0 + x1) / 2.0 * scale;
    let oy = FB_H as f32 * 0.42 - (y0 + y1) / 2.0 * scale;

    let mut cv = Canvas::new();
    for st in strokes {
        for seg in st {
            let n = 96;
            for i in 0..=n {
                let t = i as f32 / n as f32;
                let pt = bezier(&seg.p[0], &seg.p[1], &seg.p[2], &seg.p[3], t);
                let x = ox - pt[0] * scale; // mirrored design space
                let y = pt[1] * scale + oy;
                // component 3 is the stroke width (~28..30 in real data);
                // component 2 carries signed tangential values, not width
                let width = if pt[3] > 0.0 { pt[3] } else { 30.0 };
                let r = (width * scale * 0.5).max(0.6);
                cv.dot(x, y, r);
            }
        }
    }
    cv.fb
}

/// Render the welcome framebuffer from raw CursiveHello.json bytes.
pub fn welcome_fb_from_json(json: &[u8]) -> Result<Vec<u8>> {
    let strokes = parse_strokes(json)?;
    Ok(render_strokes(&strokes))
}

/// Serial marker printed by the hello kernel (asserted by verdicts/CI).
pub const SERIAL_MARKER: &str = "SWEET-RUST hello screen drawn";
