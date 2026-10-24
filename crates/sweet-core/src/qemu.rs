//! Boot a SWEETXNU payload in QEMU's `virt` board — Rust port of
//! `tools/qemu-sweet.py`, including the QMP dual transport (unix on POSIX,
//! TCP loopback on Windows), the framebuffer PNG dump, the exact-glyph
//! console decoder and the verdict/exit-code protocol. Adds an optional
//! Apple-logo loader device (`-device loader,addr=0x9c000000`).

use crate::aarch64::{u32le, u64le};
use crate::payload::{build_image, payload_data, stub, PlacedImage};
use crate::{bail, FB_BASE, FB_H, FB_STRIDE, FB_W, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const STUB_PA: u64 = 0x4020_0000; // above the dtb QEMU places at RAM base

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub secs: f64,
    pub gdb: bool,
    pub cpu: String,
    pub hmp: Vec<String>,
    pub fail_on_panic: bool,
    pub live_serial: bool,
    pub qemu_datadir: Vec<String>,
    /// Apple-logo BGRA blob loaded at 0x9c000000 before the CPU starts.
    pub logo: Option<Vec<u8>>,
    /// When set, the guest UART is a socket chardev instead of a log file
    /// (control-plane sessions type into the guest and capture output live).
    pub serial_sock: Option<PathBuf>,
    /// When set, guest RAM is a SHARED memory file (memory-backend-file with
    /// share=on): the host paints the framebuffer / screenshots by file I/O
    /// at (PA - 0x40000000), no QMP/gdb round trips.
    pub ram_file: Option<PathBuf>,
    /// When set (address of xnu's `_msgbufp`, from `llvm-nm KERNEL | grep
    /// _msgbufp`), the runner dumps the kernel console ring at stop time:
    /// x /1gx -> struct msgbuf address, x /3gx -> bufc (3rd quad), then
    /// HMP memsave of 0x20000 bytes to msgbuf.bin. fb.txt is only the LAST
    /// screen; the msgbuf holds everything printed before it (upstream
    /// NOTES.md 0038 trick). Development kernels do not slide: the nm
    /// address is the runtime VA (upstream verified across boots).
    pub msgbufp: Option<u64>,
    /// Post-mortem: pmemsave the ADT's RAMDisk (md0) to <outdir>/md0-dump.img
    /// right before QEMU quits. Launchd and daemons write TEXT logs to files
    /// on the ramdisk; the console only ever shows kernel panics.
    pub dump_ramdisk: bool,
    /// When set (seconds), pmemsave the framebuffer to fb-NNNN.png every N
    /// seconds WHILE THE GUEST RUNS. fb.png is only the LAST console screen;
    /// launchd's os_log mirror floods it in a spawn-retry cycle, so a single
    /// end-of-boot screenshot loses everything but the tail. Interval snaps
    /// give the launchd log a TIMELINE in artifacts (each decodes to text
    /// via screen-decode --kernel). No QMP conflicts: each snap is a fresh
    /// sequential connection, closed before the next.
    pub snap_every: Option<f64>,
    /// Attach the qemu-sweet device models (M4/M5 display path):
    ///   -device sweet-dart,id=sweet-dart0      (dart-disp0 @ 0x18704000)
    ///   -device sweet-mipi-dsim                (mipi-dsim @ 0x18400000)
    ///   -device sweet-disp0,dart=sweet-dart0   (disp0 @ 0x18000000, DMA
    ///                                           through the DART's IOMMU AS)
    /// Requires the qemu-sweet build (qemu-sweet/build.sh) FIRST in PATH —
    /// the devices only exist in that fork; stock QEMU errors on them.
    pub sweet_devices: bool,
    /// Serve the sweet-disp0 console over VNC (e.g. "127.0.0.1:5901" or
    /// ":1").  Requires the fork build (the console is registered by the
    /// sweet-disp0 device model) and --sweet-devices.  VNC is display
    /// OUT for now (watching the framebuffer); touch/keyboard IN arrives
    /// with the W5 BackBoard/HID work, and QMP screendump gives the
    /// artifacts regardless.
    pub vnc: Option<String>,
    /// Expose the qtest unix socket at this path (`-qtest unix:...`):
    /// the device-model gate (tools/dart-walker-gate.py) drives the
    /// sweet-dart/disp0 registers from the host through it -- the W3
    /// "guest-programmed PTE translates a DMA" measurement without a
    /// kernel-side driver (none exists: W2).  Harmless otherwise.
    pub qtest: Option<PathBuf>,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            secs: 20.0,
            gdb: false,
            cpu: "cortex-a76".into(),
            hmp: Vec::new(),
            fail_on_panic: false,
            live_serial: false,
            serial_sock: None,
            ram_file: None,
            msgbufp: None,
            dump_ramdisk: false,
            snap_every: None,
            sweet_devices: false,
            vnc: None,
            qtest: None,
            qemu_datadir: Vec::new(),
            logo: None,
        }
    }
}

/// Minimal JSON value (std-only; the pipeline only needs tiny documents).
pub mod jsonlite {
    use std::collections::BTreeMap;
    use std::fmt::Write;

    #[derive(Debug, Clone, PartialEq)]
    pub enum J {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<J>),
        Obj(BTreeMap<String, J>),
    }

    impl J {
        pub fn obj() -> J {
            J::Obj(BTreeMap::new())
        }
        pub fn set(&mut self, k: &str, v: J) {
            if let J::Obj(m) = self {
                m.insert(k.into(), v);
            }
        }
        pub fn s(v: impl Into<String>) -> J {
            J::Str(v.into())
        }
        pub fn n(v: impl Into<f64>) -> J {
            J::Num(v.into())
        }
        pub fn b(v: bool) -> J {
            J::Bool(v)
        }

        pub fn dump(&self) -> String {
            let mut s = String::new();
            self.write(&mut s);
            s
        }

        fn write(&self, out: &mut String) {
            match self {
                J::Null => out.push_str("null"),
                J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                J::Num(n) => {
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        let _ = write!(out, "{}", *n as i64);
                    } else {
                        let _ = write!(out, "{n}");
                    }
                }
                J::Str(s) => write_str(s, out),
                J::Arr(a) => {
                    out.push('[');
                    for (i, v) in a.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        v.write(out);
                    }
                    out.push(']');
                }
                J::Obj(m) => {
                    out.push('{');
                    for (i, (k, v)) in m.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        write_str(k, out);
                        out.push(':');
                        v.write(out);
                    }
                    out.push('}');
                }
            }
        }

        pub fn parse(s: &str) -> Option<J> {
            let b: Vec<char> = s.chars().collect();
            let mut i = 0usize;
            let v = parse_val(&b, &mut i)?;
            Some(v)
        }
    }

    fn write_str(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    fn parse_val(b: &[char], i: &mut usize) -> Option<J> {
        skip_ws(b, i);
        if *i >= b.len() {
            return None;
        }
        match b[*i] {
            '{' => {
                *i += 1;
                let mut m = BTreeMap::new();
                skip_ws(b, i);
                if *i < b.len() && b[*i] == '}' {
                    *i += 1;
                    return Some(J::Obj(m));
                }
                loop {
                    skip_ws(b, i);
                    let k = match parse_val(b, i)? {
                        J::Str(s) => s,
                        _ => return None,
                    };
                    skip_ws(b, i);
                    if *i >= b.len() || b[*i] != ':' {
                        return None;
                    }
                    *i += 1;
                    let v = parse_val(b, i)?;
                    m.insert(k, v);
                    skip_ws(b, i);
                    match b.get(*i) {
                        Some(',') => *i += 1,
                        Some('}') => {
                            *i += 1;
                            return Some(J::Obj(m));
                        }
                        _ => return None,
                    }
                }
            }
            '[' => {
                *i += 1;
                let mut a = Vec::new();
                skip_ws(b, i);
                if *i < b.len() && b[*i] == ']' {
                    *i += 1;
                    return Some(J::Arr(a));
                }
                loop {
                    let v = parse_val(b, i)?;
                    a.push(v);
                    skip_ws(b, i);
                    match b.get(*i) {
                        Some(',') => *i += 1,
                        Some(']') => {
                            *i += 1;
                            return Some(J::Arr(a));
                        }
                        _ => return None,
                    }
                }
            }
            '"' => {
                *i += 1;
                let mut s = String::new();
                while *i < b.len() {
                    match b[*i] {
                        '"' => {
                            *i += 1;
                            return Some(J::Str(s));
                        }
                        '\\' => {
                            *i += 1;
                            match b.get(*i) {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some('r') => s.push('\r'),
                                Some('u') => {
                                    let hex: String =
                                        b[*i + 1..(*i + 5).min(b.len())].iter().collect();
                                    if let Ok(n) = u32::from_str_radix(&hex, 16) {
                                        if let Some(c) = char::from_u32(n) {
                                            s.push(c);
                                        }
                                    }
                                    *i += 4;
                                }
                                Some(c) => s.push(*c),
                                None => return None,
                            }
                            *i += 1;
                        }
                        c => {
                            s.push(c);
                            *i += 1;
                        }
                    }
                }
                None
            }
            't' => {
                *i += 4;
                Some(J::Bool(true))
            }
            'f' => {
                *i += 5;
                Some(J::Bool(false))
            }
            'n' => {
                *i += 4;
                Some(J::Null)
            }
            _ => {
                let start = *i;
                while *i < b.len()
                    && (b[*i].is_ascii_digit()
                        || b[*i] == '-'
                        || b[*i] == '+'
                        || b[*i] == '.'
                        || b[*i] == 'e'
                        || b[*i] == 'E')
                {
                    *i += 1;
                }
                let t: String = b[start..*i].iter().collect();
                t.parse::<f64>().ok().map(J::Num)
            }
        }
    }

    fn skip_ws(b: &[char], i: &mut usize) {
        while *i < b.len() && b[*i].is_whitespace() {
            *i += 1;
        }
    }
}

pub use jsonlite::J;

// ---------------------------------------------------------------------------
// Environment checks
// ---------------------------------------------------------------------------

pub fn find_qemu() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "qemu-system-aarch64.exe" } else { "qemu-system-aarch64" };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(exe);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

pub fn check_env(cpu: &str) -> bool {
    let Some(qemu) = find_qemu() else {
        eprintln!("[!] Error: qemu-system-aarch64 not found in PATH.");
        eprintln!("    Install instructions:");
        eprintln!("      Arch Linux:    sudo pacman -S qemu-system-aarch64");
        eprintln!("      Ubuntu/Debian: sudo apt install qemu-system-arm");
        eprintln!("      Fedora:        sudo dnf install qemu-system-aarch64");
        eprintln!("      macOS:         brew install qemu");
        eprintln!("      Windows:       https://www.qemu.org/download/#windows (add the install dir to PATH)");
        eprintln!("                     see docs/WINDOWS.md for the full Windows walkthrough");
        return false;
    };
    match Command::new(&qemu).arg("--version").output() {
        Ok(o) => {
            let first = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string();
            eprintln!("[*] Found QEMU: {first}");
        }
        Err(e) => {
            eprintln!("[!] Warning: Failed to query QEMU version: {e}");
            return false;
        }
    }
    if let Ok(list) = Command::new(&qemu).args(["-M", "virt", "-cpu", "help"]).output() {
        let list = String::from_utf8_lossy(&list.stdout);
        if list.contains(cpu) {
            eprintln!("[*] CPU model {cpu} supported");
        } else {
            eprintln!("[!] Warning: CPU model {cpu} not explicitly listed in -cpu help");
        }
    }
    true
}

// ---------------------------------------------------------------------------
// QMP (dual transport)
// ---------------------------------------------------------------------------

pub enum Transport {
    Unix(PathBuf),
    Tcp(String, u16),
}

#[cfg(unix)]
pub struct Qmp {
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
fn connect(t: &Transport) -> Result<Qmp> {
    use std::os::unix::net::UnixStream;
    let stream = match t {
        Transport::Unix(p) => UnixStream::connect(p)?,
        Transport::Tcp(host, port) => {
            bail!("TCP QMP transport is only used on Windows; requested {host}:{port}")
        }
    };
    Ok(Qmp { reader: BufReader::new(stream.try_clone()?), writer: stream })
}

#[cfg(windows)]
pub struct Qmp {
    reader: BufReader<std::net::TcpStream>,
    writer: std::net::TcpStream,
}

#[cfg(windows)]
fn connect(t: &Transport) -> Result<Qmp> {
    let stream = match t {
        Transport::Tcp(host, port) => std::net::TcpStream::connect((host.as_str(), *port))?,
        Transport::Unix(_) => bail!("unix QMP transport is not available on Windows"),
    };
    Ok(Qmp { reader: BufReader::new(stream.try_clone()?), writer: stream })
}

impl Qmp {
    pub fn connect(t: &Transport) -> Result<Qmp> {
        connect(t)
    }
}

impl Qmp {
    fn next_result(&mut self) -> Result<J> {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                bail!("QMP connection closed");
            }
            if let Some(v) = J::parse(&line) {
                let is_result = matches!(&v, J::Obj(m) if m.contains_key("return") || m.contains_key("error"));
                if is_result {
                    return Ok(v);
                }
                // events and the greeting are skipped
            }
        }
    }

    pub fn handshake(&mut self) -> Result<()> {
        // Consume exactly one greeting line (python: f.readline() before the
        // command loop) — next_result() would block waiting for a response
        // that only comes after qmp_capabilities.
        let mut greeting = String::new();
        let n = self.reader.read_line(&mut greeting)?;
        if n == 0 {
            bail!("QMP connection closed before greeting");
        }
        self.call("qmp_capabilities", J::obj())?;
        Ok(())
    }

    pub fn call(&mut self, execute: &str, args: J) -> Result<J> {
        let mut req = J::obj();
        req.set("execute", J::s(execute));
        req.set("arguments", args);
        self.writer.write_all(req.dump().as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.next_result()
    }

    /// human-monitor-command wrapper (synchronous HMP via QMP).
    pub fn hmp(&mut self, cmdline: &str) -> Result<String> {
        let mut args = J::obj();
        args.set("command-line", J::s(cmdline));
        let r = self.call("human-monitor-command", args)?;
        match r {
            J::Obj(m) => match m.get("return") {
                Some(J::Str(s)) => Ok(s.clone()),
                Some(other) => Ok(other.dump()),
                Some(_) | None => Ok(String::new()),
            },
            _ => Ok(String::new()),
        }
    }
}

/// Dump xnu's console ring (the `msgbuf`) to `file` via HMP while the VM is
/// stopped. `msgbufp` is the kernel-symbol address of `_msgbufp` (a
/// `struct msgbuf *`): `x /1gx` reads the struct pointer, `x /3gx` reads
/// `{bufx?, size?, bufc}` — bufc (3rd quad) points at the ring buffer
/// (upstream NOTES.md boot-0038 recipe; static across boots, dev kernels
/// do not slide). HMP memsave translates the kernel VA through the CPU's
/// page tables.
fn dump_msgbuf(q: &mut Qmp, msgbufp: u64, file: &str) -> Result<()> {
    // pointer -> struct msgbuf
    let out1 = q.hmp(&format!("x /1gx {msgbufp:#x}"))?;
    let struct_addr = parse_hmp_quad(&out1)
        .ok_or_else(|| sc_error(format!("x /1gx gave no quad: {out1:?}")))?;
    // struct: { ... , ..., bufc } — bufc is the 3rd quad
    let out2 = q.hmp(&format!("x /3gx {struct_addr:#x}"))?;
    let quads = parse_hmp_quads(&out2);
    if quads.len() < 3 {
        return Err(sc_error(format!("x /3gx gave {} quads: {out2:?}", quads.len())));
    }
    let bufc = quads[2];
    // ring size 0x20000 (128 KiB, the stock xnu msgbuf pool)
    let r = q.hmp(&format!("memsave {bufc:#x} 0x20000 {file}"))?;
    if !r.trim().is_empty() {
        eprintln!("memsave note: {r}");
    }
    Ok(())
}

/// First "0x...: 0xVALUE" hex value in HMP `x` output.
pub(crate) fn parse_hmp_quad(s: &str) -> Option<u64> {
    parse_hmp_quads(s).into_iter().next()
}

pub(crate) fn parse_hmp_quads(s: &str) -> Vec<u64> {
    let mut v = Vec::new();
    for line in s.lines() {
        // format: "fffffe0007b56678:  0xfffffe0007b56000  0x..." or
        //          "0xfffffe0007b56678:  0x..."
        let after_colon = match line.find(':') {
            Some(i) => &line[i + 1..],
            None => continue,
        };
        for tok in after_colon.split_whitespace() {
            let t = tok;
            // QEMU prints x-command values either "0x"-prefixed (any
            // length) or as bare hex; bare words like "a" in error text
            // must not parse, so bare tokens need >= 4 digits
            let (prefixed, digits) = match t.strip_prefix("0x").or(t.strip_prefix("0X")) {
                Some(d) => (true, d),
                None => (false, t),
            };
            if !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_hexdigit())
                && (prefixed || digits.len() >= 4)
            {
                if let Ok(val) = u64::from_str_radix(digits, 16) {
                    // the leading address quad (== the address we asked for)
                    // is on the left of the colon and already excluded
                    v.push(val);
                }
            }
        }
    }
    v
}

fn sc_error(msg: String) -> crate::error::Error {
    crate::error::Error::Msg(msg)
}

// ---------------------------------------------------------------------------
// Framebuffer text decoding (exact port of decode_screen)
// ---------------------------------------------------------------------------

/// Exact glyph matching for pe_serial.c's 2x, white-on-black fbcon.
pub fn decode_screen(raw: &[u8], font: &[u8]) -> (String, usize) {
    let mut glyphs = std::collections::HashMap::new();
    for c in 32..127u8 {
        glyphs.insert(font[c as usize * 16..(c as usize + 1) * 16].to_vec(), c as char);
    }
    let stride = FB_STRIDE as usize;
    let mut rows = Vec::new();
    let mut unknown = 0usize;
    for row in 0..(FB_H as usize / 32) {
        let mut chars = Vec::new();
        for col in 0..(FB_W as usize / 16) {
            let mut glyph = [0u8; 16];
            for y in 0..16 {
                let offset = (row * 32 + y * 2) * stride + col * 16 * 4;
                for x in 0..8 {
                    if raw[offset + x * 8..offset + x * 8 + 4] == [0xff, 0xff, 0xff, 0xff] {
                        glyph[y] |= 1 << x;
                    }
                }
            }
            let key = glyph.to_vec();
            match glyphs.get(&key) {
                Some(c) => chars.push(*c),
                None => {
                    chars.push('?');
                    unknown += 1;
                }
            }
        }
        rows.push(chars.into_iter().collect::<String>().trim_end().to_string());
    }
    (
        rows.into_iter().map(|r| r.trim_end().to_string()).collect::<Vec<_>>().join("\n").trim_end().to_string() + "\n",
        unknown,
    )
}

pub fn screen_result(screen: &str, unknown: usize) -> J {
    let status = if screen.contains("panic(cpu ") || screen.contains("SWEET EARLY PANIC:") {
        "panic"
    } else if unknown > 0 {
        "incomplete_decode"
    } else if !screen.trim().is_empty() {
        "no_panic_observed"
    } else {
        "blank_screen"
    };
    let mut r = J::obj();
    r.set("status", J::s(status));
    r.set("unknown_glyphs", J::n(unknown as f64));
    r.set("snapshot_only", J::b(true));
    r
}

// ---------------------------------------------------------------------------
// Serial streaming
// ---------------------------------------------------------------------------

fn stream_serial(serial_path: &Path, file_pos: &mut u64, out: &mut impl Write) {
    if !serial_path.exists() {
        return;
    }
    if let Ok(mut f) = std::fs::File::open(serial_path) {
        use std::io::Seek;
        if f.seek(std::io::SeekFrom::Start(*file_pos)).is_err() {
            return;
        }
        let mut data = String::new();
        if f.read_to_string(&mut data).is_ok() && !data.is_empty() {
            let _ = out.write_all(data.as_bytes());
            let _ = out.flush();
            if let Ok(pos) = f.stream_position() {
                *file_pos = pos;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The boot itself
// ---------------------------------------------------------------------------

/// Live-interval framebuffer snapshot: pmemsave the FB (no VM stop — the
/// guest keeps running) and encode PNG.  Takes the CALLER'S Qmp handle:
/// the QMP chardev serves ONE client at a time (a second connection sits
/// in the backlog without a greeting, hanging forever), so every helper
/// reuses the connection it is given instead of opening its own.
fn snap_fb(q: &mut Qmp, out: &Path, at_secs: u64) -> Result<PathBuf> {
    let raw_name = format!("fb-{at_secs:04}.raw");
    let mut args = J::obj();
    args.set("val", J::n(FB_BASE as f64));
    args.set("size", J::n((FB_STRIDE * FB_H) as f64));
    args.set("filename", J::s(out.join(&raw_name).display().to_string()));
    if let J::Obj(m) = q.call("pmemsave", args)? {
        if let Some(J::Obj(e)) = m.get("error") {
            return Err(crate::Error::Msg(format!("qmp pmemsave: {e:?}")));
        }
    }
    let raw = std::fs::read(out.join(&raw_name))?;
    let png = crate::png::encode_fb(FB_W, FB_H, &raw)?;
    let png_path = out.join(format!("fb-{at_secs:04}.png"));
    std::fs::write(&png_path, &png)?;
    let _ = std::fs::remove_file(out.join(&raw_name));
    Ok(png_path)
}

/// Parse a binary PPM (P6) as written by QEMU's ppm_save: "P6\n<w> <h>\n
/// <maxval>\n" then raw RGB bytes.
fn parse_ppm6(b: &[u8]) -> Result<(u32, u32, &[u8])> {
    if b.len() < 3 || &b[0..2] != b"P6" {
        bail!("screendump: not a P6 ppm");
    }
    let mut i = 2usize;
    let mut vals = [0u32; 3];
    for v in &mut vals {
        // skip whitespace + comments between tokens
        loop {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && b[i] == b'#' {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            break;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            bail!("screendump: bad ppm header");
        }
        let txt = std::str::from_utf8(&b[start..i])
            .map_err(|_| crate::Error::Msg("bad ppm number".into()))?;
        *v = txt.parse::<u32>().map_err(|_| {
            crate::Error::Msg("bad ppm number".into())
        })?;
    }
    // exactly one whitespace byte separates the header from the pixels
    i += 1;
    let (w, h, maxval) = (vals[0], vals[1], vals[2]);
    if maxval != 255 {
        bail!("screendump: unexpected ppm maxval {maxval}");
    }
    let need = (w as usize) * (h as usize) * 3;
    if b.len() < i + need {
        bail!(
            "screendump: ppm truncated (need {need}, have {})",
            b.len() - i
        );
    }
    Ok((w, h, &b[i..i + need]))
}

/// Console screenshot through QEMU's display layer -- exactly what VNC
/// serves: QMP screendump on the sweet-disp0 console.  PNG when the fork
/// build has libpng; otherwise PPM, re-encoded to PNG here so the
/// artifact contract (PNG in out/) stays uniform.
fn snap_console(q: &mut Qmp, out: &Path, tag: &str) -> Result<PathBuf> {
    let png_path = out.join(format!("screenshot-{tag}.png"));
    let mut args = J::obj();
    args.set("filename", J::s(png_path.display().to_string()));
    args.set("format", J::s("png"));
    if let J::Obj(m) = q.call("screendump", args)? {
        if m.get("error").is_none() {
            return Ok(png_path);
        }
    }
    // no libpng in the fork -> ppm fallback
    let ppm_path = out.join(format!("screenshot-{tag}.ppm"));
    let mut args = J::obj();
    args.set("filename", J::s(ppm_path.display().to_string()));
    if let J::Obj(m) = q.call("screendump", args)? {
        if let Some(J::Obj(e)) = m.get("error") {
            return Err(crate::Error::Msg(format!("qmp screendump: {e:?}")));
        }
    }
    let ppm = std::fs::read(&ppm_path)?;
    let (w, h, rgb) = parse_ppm6(&ppm)?;
    let png = crate::png::encode_rgb(w, h, rgb)?;
    std::fs::write(&png_path, &png)?;
    let _ = std::fs::remove_file(&ppm_path);
    Ok(png_path)
}

pub struct BootOutputs {
    pub fb_png: PathBuf,
    pub regs_txt: PathBuf,
    pub qemu_log: PathBuf,
    pub serial_txt: PathBuf,
    pub result_json: PathBuf,
    pub result: J,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    payload_path: &Path,
    outdir: &Path,
    opts: &RunOptions,
    font: Option<Vec<u8>>,
) -> Result<BootOutputs> {
    if !check_env(&opts.cpu) {
        bail!("QEMU environment check failed");
    }
    let payload = std::fs::read(payload_path)?;
    let out = std::fs::canonicalize(outdir).unwrap_or_else(|_| outdir.to_path_buf());
    std::fs::create_dir_all(&out)?;

    let PlacedImage { phys_base, image: img, boot_args_pa, entry_pa, mem_size, ramdisk } =
        build_image(&payload)?;
    // QEMU's generic loader rejects files >= 2GiB (get_image_size() is a
    // signed int) - split the image into chunks loaded at consecutive PAs.
    let loader_parts = write_loader_chunks(&out, "ram.bin", &img, phys_base)?;
    let stub_bin = out.join("stub.bin");
    std::fs::write(&stub_bin, stub(boot_args_pa, entry_pa))?;
    eprintln!(
        "phys_base={:#x} boot_args={boot_args_pa:#x} entry={entry_pa:#x}",
        phys_base
    );
    drop(img);

    // QMP transport: unix socket on POSIX; TCP loopback on Windows.
    let (mon, mon_arg) = if cfg!(windows) {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = probe.local_addr()?.port();
        drop(probe);
        (Transport::Tcp("127.0.0.1".into(), port), format!("tcp:127.0.0.1:{port},server,nowait"))
    } else {
        let sock = out.join("mon.sock");
        if sock.exists() {
            let _ = std::fs::remove_file(&sock);
        }
        (Transport::Unix(sock.clone()), format!("unix:{},server,nowait", sock.display()))
    };
    // virt RAM starts at 0x40000000: cover the payload's RAM (python clamps
    // to >= 4G; the low-RAM override ADT only needs 2G, so clamp to 2 here —
    // the standard QEMU ADT still computes 4-5G).
    let gib = ((phys_base + mem_size).saturating_sub(0x4000_0000)).div_ceil(1 << 30);
    let ram_gib = gib.max(2);
    let mut cmd = Command::new(find_qemu().ok_or_else(|| crate::Error::Msg("no qemu".into()))?);
    cmd.args([
        "-M",
        "virt,gic-version=3",
        "-cpu",
        &opts.cpu,
        "-smp",
        "1",
        "-m",
        &format!("{ram_gib}G"),
        "-nographic",
        // No guest NIC: Debian builds map the implicit default NIC to
        // virtio-net-pci and then die on a missing efi-virtio.rom. The boot
        // pipeline is serial+QMP only, so remove the device entirely.
        "-nic",
        "none",
        "-serial",
        &format!("file:{}", out.join("serial.txt").display()),
        "-qmp",
        &mon_arg,
        "-d",
        "int,guest_errors,unimp",
        "-D",
        &out.join("qemu.log").display().to_string(),
        "-no-reboot",
    ]);
    // qemu-sweet M4/M5 device models: DART first (disp0 links to it by id),
    // then MIPI-DSI, then the display controller with its DMA address space
    // rooted at the DART's IOMMU. Order in the log proves the chain.
    if opts.sweet_devices {
        cmd.args([
            "-device",
            "sweet-dart,id=sweet-dart0",
            "-device",
            "sweet-mipi-dsim",
            "-device",
            "sweet-disp0,dart=sweet-dart0",
        ]);
    }
    // VNC display out (fork-only: the console is registered by
    // sweet-disp0).  Coexists with -nographic: the VNC server is a
    // separate frontend from the default display.
    if let Some(vnc) = &opts.vnc {
        cmd.args(["-vnc", vnc]);
    }
    // qtest socket (device-model gate: see RunOptions::qtest).
    if let Some(qt) = &opts.qtest {
        cmd.args([
            "-qtest",
            &format!("unix:{},server,nowait", qt.display()),
            "-qtest-log",
            "none",
        ]);
    }
    // payload image: one loader device per <=1.5GiB chunk, consecutive PAs
    for (p, off) in &loader_parts {
        cmd.args([
            "-device",
            &format!(
                "loader,file={},addr={:#x},force-raw=on",
                p.display(),
                phys_base + off
            ),
        ]);
    }
    cmd.args([
        "-device",
        &format!("loader,file={},addr={STUB_PA:#x},force-raw=on", stub_bin.display()),
        "-device",
        &format!("loader,addr={STUB_PA:#x},cpu-num=0"),
    ]);
    // Apple-logo asset: fill the framebuffer before the CPU starts.
    let mut logo_file = None;
    if let Some(logo) = &opts.logo {
        let lf = out.join("logo.bin");
        std::fs::write(&lf, logo)?;
        cmd.args([
            "-device",
            &format!("loader,file={},addr={FB_BASE:#x},force-raw=on", lf.display()),
        ]);
        logo_file = Some(lf);
    }
    for d in &opts.qemu_datadir {
        cmd.args(["-L", d]);
    }
    if opts.gdb {
        cmd.args(["-S", "-gdb", "tcp::1234"]);
    }
    cmd.stdin(Stdio::null());

    let serial_path = out.join("serial.txt");
    let mut serial_pos = 0u64;
    let mut p: Child = cmd.spawn()?;
    let t0 = Instant::now();
    let mut stdout = std::io::stdout();
    let mut last_snap = 0f64;
    while p.try_wait()?.is_none() && t0.elapsed() < Duration::from_secs_f64(opts.secs) {
        if opts.live_serial {
            stream_serial(&serial_path, &mut serial_pos, &mut stdout);
        }
        if let Some(every) = opts.snap_every {
            let el = t0.elapsed().as_secs_f64();
            // leave the last `every` seconds to the end-of-boot screenshot
            if el - last_snap >= every && el < opts.secs - every {
                last_snap = el;
                // ONE fresh connection per snap tick, dropped before the
                // next (single-connection discipline: the QMP chardev
                // serves one client at a time -- see snap_fb).
                let mut sq = match Qmp::connect(&mon) {
                    Ok(mut q) => match q.handshake() {
                        Ok(()) => q,
                        Err(e) => {
                            eprintln!("snap connect failed: {e}");
                            continue;
                        }
                    },
                    Err(e) => {
                        eprintln!("snap connect failed: {e}");
                        continue;
                    }
                };
                match snap_fb(&mut sq, &out, el as u64) {
                    Ok(path) => eprintln!("snap: {}", path.display()),
                    Err(e) => eprintln!("snap failed: {e}"),
                }
                if opts.sweet_devices {
                    match snap_console(&mut sq, &out, &format!("{:04}", el as u64)) {
                        Ok(path) => eprintln!("screenshot: {}", path.display()),
                        Err(e) => eprintln!("screenshot failed: {e}"),
                    }
                }
                drop(sq);
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if opts.live_serial {
        stream_serial(&serial_path, &mut serial_pos, &mut stdout);
    }

    let mut result = J::obj();
    let mut qemu_rc: Option<i32> = None;
    if let Some(status) = p.try_wait()? {
        qemu_rc = status.code();
    }

    if qemu_rc.is_none() {
        let mut q = Qmp::connect(&mon)?;
        q.handshake()?;
        q.hmp("stop")?;
        for h in &opts.hmp {
            let r = q.hmp(h)?;
            eprintln!("(qemu) {h}\n{r}");
        }
        let regs = q.hmp("info registers")?;
        // native QMP pmemsave (HMP's parser chokes on '-' in paths)
        let mut args = J::obj();
        args.set("val", J::n(FB_BASE as f64));
        args.set("size", J::n((FB_STRIDE * FB_H) as f64));
        args.set("filename", J::s(out.join("fb.raw").display().to_string()));
        if let J::Obj(m) = q.call("pmemsave", args)? {
            if let Some(J::Obj(e)) = m.get("error") {
                eprintln!("qmp pmemsave: {e:?}");
            }
        }
        // kernel console ring dump (see RunOptions::msgbufp): x /1gx reads
        // the struct msgbuf pointer, x /3gx reads {.., .., bufc}, memsave
        // pulls the ring. HMP `x` prints "0x....: 0xVALUE" per quad.
        if let Some(mb) = opts.msgbufp {
            match dump_msgbuf(&mut q, mb, "msgbuf.bin") {
                Ok(()) => eprintln!("msgbuf: dumped 0x20000 bytes to msgbuf.bin"),
                Err(e) => eprintln!("msgbuf: dump failed: {e}"),
            }
        }
        if opts.dump_ramdisk {
            if let Some((rd_pa, rd_size)) = ramdisk {
                let mut args = J::obj();
                args.set("val", J::n(rd_pa as f64));
                args.set("size", J::n(rd_size as f64));
                args.set("filename", J::s(out.join("md0-dump.img").display().to_string()));
                if let J::Obj(m) = q.call("pmemsave", args)? {
                    if let Some(J::Obj(e)) = m.get("error") {
                        eprintln!("qmp pmemsave md0: {e:?}");
                    } else {
                        eprintln!(
                            "md0 dump: {rd_size} bytes @ {rd_pa:#x} -> {}",
                            out.join("md0-dump.img").display()
                        );
                    }
                }
            } else {
                eprintln!("--dump-ramdisk: payload has no RAMDisk entry");
            }
        }
        // console screenshot of the final screen (the display layer's
        // view -- what VNC serves) WHILE QEMU IS STILL ALIVE, on the SAME
        // persistent connection (a second connection would hang: the QMP
        // chardev serves one client at a time).  The console exists only
        // with the fork's devices attached.
        if opts.sweet_devices {
            match snap_console(&mut q, &out, "final") {
                Ok(path) => {
                    eprintln!("screenshot: {}", path.display());
                    result.set(
                        "console_screenshot",
                        J::s(path.display().to_string()),
                    );
                }
                Err(e) => eprintln!("screenshot failed: {e}"),
            }
        }
        let _ = p.kill();
        let _ = p.wait();

        let regs_txt = out.join("regs.txt");
        std::fs::write(&regs_txt, &regs)?;
        let fb_raw = std::fs::read(out.join("fb.raw"))?;
        let fb_png = crate::png::encode_fb(FB_W, FB_H, &fb_raw)?;
        std::fs::write(out.join("fb.png"), &fb_png)?;
        result.set("status", J::s("not_decoded"));
        if let Some(font) = font {
            let (screen, unknown) = decode_screen(&fb_raw, &font);
            std::fs::write(out.join("fb.txt"), &screen)?;
            result = screen_result(&screen, unknown);
        }
        // human-readable console history (NULs stripped); memsave used a
        // dash-free relative name (HMP parser limitation), move into out/
        let mb_tmp = std::path::Path::new("msgbuf.bin");
        if mb_tmp.exists() {
            let mb = out.join("msgbuf.bin");
            let _ = std::fs::rename(mb_tmp, &mb);
            let raw = std::fs::read(&mb)?;
            let txt: Vec<u8> = raw.into_iter().filter(|&b| b != 0).collect();
            std::fs::write(out.join("msgbuf.txt"), &txt)?;
        }
        let _ = std::fs::remove_file(out.join("fb.raw"));
    } else {
        eprintln!("qemu exited early: {:?}", qemu_rc);
        result.set("status", J::s("qemu_exited"));
        result.set("qemu_returncode", J::n(qemu_rc.unwrap_or(-1) as f64));
    }
    if let Some(vnc) = &opts.vnc {
        result.set("vnc", J::s(vnc.clone()));
    }
    std::fs::write(out.join("result.json"), format!("{}\n", result.dump()))?;
    for (p, _) in &loader_parts {
        let _ = std::fs::remove_file(p);
    }
    if let Some(lf) = logo_file {
        let _ = std::fs::remove_file(lf);
    }
    eprintln!("{}", result.dump());
    eprintln!(
        "{} {} {}",
        out.join("fb.png").display(),
        out.join("regs.txt").display(),
        out.join("qemu.log").display()
    );

    let status = match &result {
        J::Obj(m) => match m.get("status") {
            Some(J::Str(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    if opts.fail_on_panic && status == "panic" {
        bail!("EXIT2: visible panic");
    }
    if opts.fail_on_panic && (status == "incomplete_decode" || status == "blank_screen") {
        bail!("EXIT3: undecodable or blank screen");
    }
    if status == "qemu_exited" {
        bail!("EXIT1: qemu exited early");
    }
    Ok(BootOutputs {
        fb_png: out.join("fb.png"),
        regs_txt: out.join("regs.txt"),
        qemu_log: out.join("qemu.log"),
        serial_txt: serial_path,
        result_json: out.join("result.json"),
        result,
    })
}

/// QEMU's generic loader rejects files >= 2GiB (get_image_size() returns a
/// signed int). Split the boot image into <=1.5GiB chunks; each chunk gets
/// its own `-device loader` at phys_base + offset (guest RAM ends up
/// byte-identical to the single-file case).
const LOADER_CHUNK: usize = 1_500_000_000;

fn write_loader_chunks(
    out: &Path,
    base: &str,
    img: &[u8],
    phys_base: u64,
) -> Result<Vec<(std::path::PathBuf, u64)>> {
    let _ = phys_base;
    let n = img.len().div_ceil(LOADER_CHUNK).max(1);
    let mut parts = Vec::with_capacity(n);
    for i in 0..n {
        let s = i * LOADER_CHUNK;
        let e = (s + LOADER_CHUNK).min(img.len());
        let p = out.join(format!("{base}.{i:03}"));
        std::fs::write(&p, &img[s..e])?;
        parts.push((p, (i * LOADER_CHUNK) as u64));
    }
    Ok(parts)
}

/// Extract the `_iso_font` glyph table from an unstripped kernel (native
/// LC_SYMTAB parse first; llvm-nm only as the `--nm` fallback).
pub fn extract_font(kernel: &Path, payload: &[u8], nm: &str) -> Result<Vec<u8>> {
    // Preferred: the NATIVE symtab parse (W2: llvm-nm is not installed in
    // every sandbox/CI image and the old shell-out failed silently; the
    // kernel's LC_SYMTAB carries the same `_iso_font` address).
    let mut fonts: Vec<u64> = Vec::new();
    if let Ok(buf) = std::fs::read(kernel) {
        if let Ok(syms) = crate::macho::symbols(&buf) {
            fonts = syms.iter().filter(|(n, _)| *n == "_iso_font").map(|(_, v)| *v).collect();
        }
    }
    if fonts.len() != 1 {
        // Fallback: external llvm-nm (kept for fileset/prelinked layouts the
        // native parse may not cover).
        let out = Command::new(nm).args(["--defined-only"]).arg(kernel).output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        fonts = Vec::new();
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 3 && parts[2] == "_iso_font" {
                if let Ok(v) = u64::from_str_radix(parts[0], 16) {
                    fonts.push(v);
                }
            }
        }
        if fonts.len() != 1 {
            bail!("kernel must contain exactly one _iso_font symbol (found {})", fonts.len());
        }
    }
    payload_data(payload, fonts[0], 256 * 16)
}

// silence unused warnings on non-unix where u32le/u64le are only used by tests
#[allow(dead_code)]
fn _use(a: &[u8]) -> (u32, u64) {
    (u32le(a, 0), u64le(a, 8))
}

// ---------------------------------------------------------------------------
// Interactive spawn: same pipeline as run(), but the VM stays alive and the
// control plane keeps the handles (serial socket, QMP, gdbstub).
// ---------------------------------------------------------------------------

pub struct Spawned {
    pub child: Child,
    pub mon: Transport,
    pub out: PathBuf,
    /// Unix socket of the guest UART (None on Windows: read out/serial.tcp).
    pub serial_sock: Option<PathBuf>,
    /// Shared guest RAM file when opts.ram_file was set.
    pub ram_file: Option<PathBuf>,
}

/// Spawn the payload under QEMU and return WITHOUT waiting: the caller owns
/// the running machine through Session (control.rs).
pub fn spawn_session(
    payload_path: &Path,
    outdir: &Path,
    opts: &RunOptions,
    gdb_port: u16,
) -> Result<Spawned> {
    if !check_env(&opts.cpu) {
        bail!("QEMU environment check failed");
    }
    let payload = std::fs::read(payload_path)?;
    let out = std::fs::canonicalize(outdir).unwrap_or_else(|_| outdir.to_path_buf());
    std::fs::create_dir_all(&out)?;

    let PlacedImage { phys_base, image: img, boot_args_pa, entry_pa, mem_size, .. } =
        build_image(&payload)?;
    let loader_parts = write_loader_chunks(&out, "ram.bin", &img, phys_base)?;
    let stub_bin = out.join("stub.bin");
    std::fs::write(&stub_bin, stub(boot_args_pa, entry_pa))?;
    eprintln!(
        "phys_base={:#x} boot_args={boot_args_pa:#x} entry={entry_pa:#x}",
        phys_base
    );
    drop(img);

    let (mon, mon_arg) = if cfg!(windows) {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = probe.local_addr()?.port();
        drop(probe);
        (Transport::Tcp("127.0.0.1".into(), port), format!("tcp:127.0.0.1:{port},server,nowait"))
    } else {
        let sock = out.join("mon.sock");
        if sock.exists() {
            let _ = std::fs::remove_file(&sock);
        }
        (Transport::Unix(sock.clone()), format!("unix:{},server,nowait", sock.display()))
    };
    let gib = ((phys_base + mem_size).saturating_sub(0x4000_0000)).div_ceil(1 << 30);
    let ram_gib = gib.max(2);
    let mut cmd = Command::new(find_qemu().ok_or_else(|| crate::Error::Msg("no qemu".into()))?);
    // Shared-memory guest RAM: the control plane paints/screenshots the
    // framebuffer by writing the RAM file directly (PA - RAM_BASE offset).
    let mut machine = "virt,gic-version=3".to_string();
    if let Some(ramf) = &opts.ram_file {
        if ramf.exists() {
            let _ = std::fs::remove_file(ramf);
        }
        let rf = std::fs::File::create(ramf)?;
        rf.set_len((1u64 << 30) * ram_gib as u64)?;
        drop(rf);
        machine.push_str(",memory-backend=ram0");
        cmd.args([
            "-object",
            &format!(
                "memory-backend-file,id=ram0,size={ram_gib}G,mem-path={},share=on",
                ramf.display()
            ),
        ]);
    } else {
        // plain anonymous RAM; -m is added below for both modes
    }
    // -m must match the backend size (virt checks maxmem against it)
    cmd.args([
        "-m",
        &format!("{ram_gib}G"),
        "-M",
        &machine,
        "-cpu",
        &opts.cpu,
        "-smp",
        "1",
        "-nographic",
        "-nic",
        "none",
    ]);
    // Guest UART: socket chardev (interactive) or plain log file.
    let mut serial_sock = opts.serial_sock.clone();
    if let Some(p) = &serial_sock {
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
        if cfg!(windows) {
            let port = crate::control::free_tcp_port()?;
            std::fs::write(out.join("serial.tcp"), format!("{port}\n"))?;
            cmd.args([
                "-chardev",
                &format!("socket,id=ser0,host=127.0.0.1,port={port},server=on,wait=off"),
                "-serial",
                "chardev:ser0",
            ]);
            serial_sock = None; // Windows path: Session reads out/serial.tcp
        } else {
            cmd.args([
                "-chardev",
                &format!("socket,id=ser0,path={},server=on,wait=off", p.display()),
                "-serial",
                "chardev:ser0",
            ]);
        }
    } else {
        cmd.args(["-serial", &format!("file:{}", out.join("serial.txt").display())]);
    }
    cmd.args([
        "-qmp",
        &mon_arg,
        "-d",
        "int,guest_errors,unimp",
        "-D",
        &out.join("qemu.log").display().to_string(),
        "-no-reboot",
    ]);
    for (p, off) in &loader_parts {
        cmd.args([
            "-device",
            &format!(
                "loader,file={},addr={:#x},force-raw=on",
                p.display(),
                phys_base + off
            ),
        ]);
    }
    cmd.args([
        "-device",
        &format!("loader,file={},addr={STUB_PA:#x},force-raw=on", stub_bin.display()),
        "-device",
        &format!("loader,addr={STUB_PA:#x},cpu-num=0"),
    ]);
    if let Some(logo) = &opts.logo {
        let lf = out.join("logo.bin");
        std::fs::write(&lf, logo)?;
        cmd.args([
            "-device",
            &format!("loader,file={},addr={FB_BASE:#x},force-raw=on", lf.display()),
        ]);
    }
    for d in &opts.qemu_datadir {
        cmd.args(["-L", d]);
    }
    // qemu-sweet M4/M5 device models (same wiring as run()) and VNC
    // display out.  The control plane's fb access (shared RAM file /
    // pmemsave) is independent of this; VNC is a live view for humans.
    if opts.sweet_devices {
        cmd.args([
            "-device",
            "sweet-dart,id=sweet-dart0",
            "-device",
            "sweet-mipi-dsim",
            "-device",
            "sweet-disp0,dart=sweet-dart0",
        ]);
    }
    if let Some(vnc) = &opts.vnc {
        cmd.args(["-vnc", vnc]);
    }
    // gdbstub WITHOUT -S: the machine boots immediately; gdb attaches live.
    // nodelay=on: without it every RSP packet round trip pays
    // Nagle+delayed-ACK (~40ms), turning one FB paint into ~100s.
    cmd.args(["-gdb", &format!("tcp:127.0.0.1:{gdb_port},server=on,wait=off,nodelay=on")]);
    cmd.stdin(Stdio::null());
    let child = cmd.spawn()?;
    Ok(Spawned { child, mon, out, serial_sock, ram_file: opts.ram_file.clone() })
}
