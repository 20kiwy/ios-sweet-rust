//! Live emulator control plane: boot the payload under QEMU, then steer the
//! RUNNING machine from the host:
//!   - QMP  : pause/cont, framebuffer dump (pmemsave), registers
//!   - GDB  : read/write guest memory while the VM is stopped (RSP `m`/`M`)
//!   - serial chardev socket: bidirectional console (guest RX + host capture)
//!
//! This is what turns the one-shot `ipsw-boot` into a controllable phone:
//! paint the framebuffer with real IPSW wallpaper at runtime, type into the
//! guest UART, screenshot any screen state.

use crate::qemu::{Qmp, RunOptions, Transport};
use crate::{bail, Result, FB_STRIDE};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// GDB remote serial protocol (minimal client: read/write memory)
// ---------------------------------------------------------------------------

pub struct Gdb {
    s: TcpStream,
}

impl Gdb {
    /// Attach to QEMU's gdbstub (-gdb tcp:127.0.0.1:PORT). Retries ~5s.
    pub fn attach(port: u16) -> Result<Self> {
        let t0 = Instant::now();
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    s.set_nodelay(true).ok();
                    s.set_read_timeout(Some(Duration::from_secs(20))).ok();
                    return Ok(Gdb { s });
                }
                Err(_) if t0.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn flush_acks(&mut self) {
        // consume any pending '+'/'-' acks
        let mut b = [0u8; 1];
        loop {
            match self.s.peek(&mut b) {
                Ok(1) if b[0] == b'+' || b[0] == b'-' => {
                    let _ = self.s.read(&mut b);
                }
                _ => break,
            }
        }
    }

    /// Send one packet, return the response body.
    pub fn cmd(&mut self, body: &str) -> Result<String> {
        let sum: u8 = body.bytes().fold(0u8, |a, b| a.wrapping_add(b));
        let pkt = format!("${body}#{sum:02x}");
        self.s.write_all(pkt.as_bytes())?;
        self.s.flush()?;
        // read until '#xx', skipping ack chars
        let mut resp = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            self.s.read_exact(&mut byte)?;
            match byte[0] {
                b'+' | b'-' => continue,
                b'$' => {
                    resp.clear();
                    loop {
                        self.s.read_exact(&mut byte)?;
                        if byte[0] == b'#' {
                            let mut cs = [0u8; 2];
                            self.s.read_exact(&mut cs)?;
                            // ack the reply
                            let _ = self.s.write_all(b"+");
                            let _ = self.s.flush();
                            return Ok(String::from_utf8_lossy(&resp).to_string());
                        }
                        resp.push(byte[0]);
                    }
                }
                _ => {}
            }
        }
    }

    /// Write guest memory (chunked `M` packets; QEMU caps packet length).
    /// NOTE: QEMU's gdbstub does NOT tolerate pipelined packets (a second
    /// packet arriving before its peer read the first reply is dropped), and
    /// its TCP chardev needs nodelay=on (see spawn_session) or every round
    /// trip costs ~40ms in Nagle+delayed-ACK.
    pub fn write_mem(&mut self, addr: u64, data: &[u8]) -> Result<()> {
        const CHUNK: usize = 8000; // 16000 hex chars < QEMU's 16384 packet cap
        for (i, chunk) in data.chunks(CHUNK).enumerate() {
            let mut hex = String::with_capacity(chunk.len() * 2);
            for b in chunk {
                hex.push_str(&format!("{b:02x}"));
            }
            let a = addr + (i * CHUNK) as u64;
            let r = self.cmd(&format!("M{a:x},{:x}:{hex}", chunk.len()))?;
            if r != "OK" {
                bail!("gdb M packet at {a:#x} rejected: {r:?}");
            }
        }
        Ok(())
    }

    /// Read one RSP reply (skips acks; acks each reply).
    fn read_reply(&mut self) -> Result<String> {
        let mut resp = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            self.s.read_exact(&mut byte)?;
            match byte[0] {
                b'+' | b'-' => continue,
                b'$' => {
                    resp.clear();
                    loop {
                        self.s.read_exact(&mut byte)?;
                        if byte[0] == b'#' {
                            let mut cs = [0u8; 2];
                            self.s.read_exact(&mut cs)?;
                            let _ = self.s.write_all(b"+");
                            let _ = self.s.flush();
                            return Ok(String::from_utf8_lossy(&resp).to_string());
                        }
                        resp.push(byte[0]);
                    }
                }
                _ => {}
            }
        }
    }

    /// Read guest memory.
    pub fn read_mem(&mut self, addr: u64, len: usize) -> Result<Vec<u8>> {
        let r = self.cmd(&format!("m{addr:x},{len:x}"))?;
        if r.len() < len * 2 || r.starts_with('E') {
            bail!("gdb m packet failed: {r:?}");
        }
        let mut out = Vec::with_capacity(len);
        let b = r.as_bytes();
        for i in 0..len {
            let hi = (b[i * 2] as char).to_digit(16).unwrap_or(0) as u8;
            let lo = (b[i * 2 + 1] as char).to_digit(16).unwrap_or(0) as u8;
            out.push((hi << 4) | lo);
        }
        Ok(out)
    }

    /// Read one register by GDB regnum via the `p` packet (64-bit decode).
    pub fn read_reg(&mut self, num: u32) -> Result<u64> {
        let r = self.cmd(&format!("p{num:x}"))?;
        if r.starts_with('E') || r.is_empty() {
            bail!("gdb p{num:x} failed: {r:.40}");
        }
        let b = r.as_bytes();
        let n = b.len() / 2;
        if n > 8 {
            bail!("gdb p{num:x}: {}-byte reg?", n);
        }
        let mut bytes = [0u8; 8];
        for k in 0..n {
            let hi = (b[k * 2] as char).to_digit(16).unwrap_or(0) as u8;
            let lo = (b[k * 2 + 1] as char).to_digit(16).unwrap_or(0) as u8;
            bytes[k] = (hi << 4) | lo;
        }
        Ok(u64::from_le_bytes(bytes))
    }

    /// One vCPU register sample via the `g` packet.
    ///
    /// QEMU aarch64 builtin (no XML negotiation) `g` layout: 31 GPR slots
    /// (x0..x30) as 8 little-endian bytes each, then pc (8 bytes), pstate
    /// (4 bytes), then the FPU/SIMD block. We decode the first 32 GPR
    /// slots: index 30 = lr (x30), index 31 = pc. The caller validates
    /// against the kernel text range — a wrong pc-slot choice is caught
    /// by the histogram being meaningless, and the raw samples are kept
    /// either way (honesty rules).
    pub fn regs_sample(&mut self) -> Result<[u64; 32]> {
        let r = self.cmd("g")?;
        if r.starts_with('E') || r.len() < 32 * 16 {
            bail!("gdb g packet too short: {} chars: {r:.40}", r.len());
        }
        let b = r.as_bytes();
        let mut out = [0u64; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            let mut bytes = [0u8; 8];
            for k in 0..8 {
                let hi = (b[i * 16 + k * 2] as char).to_digit(16).unwrap_or(0) as u8;
                let lo = (b[i * 16 + k * 2 + 1] as char).to_digit(16).unwrap_or(0) as u8;
                bytes[k] = (hi << 4) | lo;
            }
            *slot = u64::from_le_bytes(bytes);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Serial chardev link (socket): guest UART <-> host
// ---------------------------------------------------------------------------

pub struct SerialLink {
    stream: SerialStream,
    pub buf: Vec<u8>,
    txt: PathBuf,
    #[cfg(windows)]
    nonblocking: bool,
}

enum SerialStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(TcpStream),
}

impl SerialLink {
    #[cfg(unix)]
    fn connect_unix(p: &Path) -> Result<Self> {
        let t0 = Instant::now();
        loop {
            match std::os::unix::net::UnixStream::connect(p) {
                Ok(s) => {
                    s.set_nonblocking(true)?;
                    return Ok(SerialLink {
                        stream: SerialStream::Unix(s),
                        buf: Vec::new(),
                        txt: PathBuf::new(),
                        #[cfg(windows)]
                        nonblocking: false,
                    });
                }
                Err(_) if t0.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn connect_tcp(port: u16) -> Result<Self> {
        let t0 = Instant::now();
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    s.set_nonblocking(true)?;
                    s.set_nodelay(true).ok();
                    return Ok(SerialLink {
                        stream: SerialStream::Tcp(s),
                        buf: Vec::new(),
                        txt: PathBuf::new(),
                        #[cfg(windows)]
                        nonblocking: false,
                    });
                }
                Err(_) if t0.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Drain whatever the guest has written; append to the transcript file.
    pub fn pump(&mut self) -> Result<usize> {
        let mut total = 0usize;
        let mut tmp = [0u8; 65536];
        loop {
            let n = match &mut self.stream {
                #[cfg(unix)]
                SerialStream::Unix(s) => match s.read(&mut tmp) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                },
                SerialStream::Tcp(s) => match s.read(&mut tmp) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                },
            };
            if n == 0 {
                break;
            }
            self.buf.extend_from_slice(&tmp[..n]);
            total += n;
            if total > 16 << 20 {
                break; // safety valve
            }
        }
        if total > 0 && !self.txt.as_os_str().is_empty() {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.txt) {
                let _ = f.write_all(&self.buf[self.buf.len() - total..]);
            }
        }
        Ok(total)
    }

    /// Send bytes INTO the guest UART (RX side).
    pub fn send(&mut self, bytes: &[u8]) -> Result<()> {
        match &mut self.stream {
            #[cfg(unix)]
            SerialStream::Unix(s) => {
                s.set_nonblocking(false)?;
                s.write_all(bytes)?;
                s.flush()?;
                s.set_nonblocking(true)?;
            }
            SerialStream::Tcp(s) => {
                s.set_nonblocking(false)?;
                s.write_all(bytes)?;
                s.flush()?;
                s.set_nonblocking(true)?;
            }
        }
        Ok(())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf).to_string()
    }

    /// Pump the UART until `marker` appears or the timeout elapses.
    pub fn wait_marker(&mut self, marker: &str, timeout: f64) -> bool {
        let t0 = Instant::now();
        loop {
            let _ = self.pump();
            if self.text().contains(marker) {
                return true;
            }
            if t0.elapsed().as_secs_f64() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ---------------------------------------------------------------------------
// Emulator session: spawn + QMP + GDB + serial
// ---------------------------------------------------------------------------

pub struct Session {
    pub child: Child,
    pub mon: Transport,
    pub serial: SerialLink,
    pub gdb_port: u16,
    pub out: PathBuf,
    /// Shared guest RAM file (PA - 0x40000000 = file offset) when configured.
    pub ram_file: Option<PathBuf>,
}

/// Guest RAM base on the QEMU virt machine (the shared file maps from here).
pub const RAM_BASE: u64 = 0x4000_0000;

impl Session {
    /// Spawn QEMU with the payload (see qemu::spawn_session) and open the
    /// control channels. The VM is RUNNING when this returns.
    pub fn boot(payload: &Path, outdir: &Path, opts: &RunOptions, gdb_port: u16) -> Result<Self> {
        let spawned = crate::qemu::spawn_session(payload, outdir, opts, gdb_port)?;
        let serial = match &spawned.serial_sock {
            #[cfg(unix)]
            Some(p) => SerialLink::connect_unix(p)?,
            // exhaustiveness on non-unix hosts (spawn_session only sets the
            // unix socket on unix; the marker file below carries the port)
            #[cfg(not(unix))]
            Some(_) => unreachable!("serial_sock is never Some on non-unix"),
            None => {
                // Windows path passes the TCP port through the out dir marker
                let port_file = spawned.out.join("serial.tcp");
                let port: u16 = std::fs::read_to_string(&port_file)?
                    .trim()
                    .parse()
                    .map_err(|e| crate::Error::Msg(format!("bad serial.tcp: {e}")))?;
                SerialLink::connect_tcp(port)?
            }
        };
        let mut sl = serial;
        sl.txt = spawned.out.join("serial.txt");
        Ok(Session {
            child: spawned.child,
            mon: spawned.mon,
            serial: sl,
            gdb_port,
            out: spawned.out,
            ram_file: spawned.ram_file,
        })
    }

    fn q(&self) -> Result<Qmp> {
        let mut q = Qmp::connect(&self.mon)?;
        q.handshake()?;
        Ok(q)
    }

    pub fn pause(&self) -> Result<()> {
        self.q()?.hmp("stop")?;
        Ok(())
    }

    /// Is the VM still alive? (a QMP connect + handshake round trip)
    pub fn child_handshake_ok(&self) -> bool {
        self.q().is_ok()
    }

    pub fn cont(&self) -> Result<()> {
        self.q()?.hmp("cont")?;
        Ok(())
    }

    /// Dump the framebuffer as raw BGRA. With the shared RAM file this is a
    /// plain file read; otherwise a QMP pmemsave round trip (VM stopped).
    pub fn fb_raw(&self) -> Result<Vec<u8>> {
        let size = (FB_STRIDE * crate::FB_H) as usize;
        if let Some(ramf) = &self.ram_file {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(ramf)?;
            let off = crate::FB_BASE - RAM_BASE;
            f.seek(SeekFrom::Start(off))?;
            let mut raw = vec![0u8; size];
            f.read_exact(&mut raw)?;
            return Ok(raw);
        }
        let mut q = self.q()?;
        q.hmp("stop")?;
        let tmp = self.out.join("fb-snap.raw");
        let mut args = crate::qemu::J::obj();
        args.set("val", crate::qemu::J::n(crate::FB_BASE as f64));
        args.set("size", crate::qemu::J::n((FB_STRIDE * crate::FB_H) as f64));
        args.set("filename", crate::qemu::J::s(tmp.display().to_string()));
        if let crate::qemu::J::Obj(m) = q.call("pmemsave", args)? {
            if let Some(crate::qemu::J::Obj(e)) = m.get("error") {
                q.hmp("cont")?;
                bail!("pmemsave failed: {e:?}");
            }
        }
        q.hmp("cont")?;
        let raw = std::fs::read(&tmp)?;
        let _ = std::fs::remove_file(&tmp);
        Ok(raw)
    }

    /// Screenshot the live screen to a PNG; returns (path, raw BGRA).
    pub fn shot(&self, name: &str) -> Result<(PathBuf, Vec<u8>)> {
        let raw = self.fb_raw()?;
        let png = crate::png::encode_fb(crate::FB_W, crate::FB_H, &raw)?;
        let p = self.out.join(name);
        std::fs::write(&p, &png)?;
        Ok((p, raw))
    }

    /// Write bytes into guest RAM while the VM is stopped (GDB RSP).
    pub fn poke(&self, addr: u64, data: &[u8]) -> Result<()> {
        // fast path: shared RAM file, no gdb round trips
        if let Some(ramf) = &self.ram_file {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(ramf)?;
            f.seek(SeekFrom::Start(addr - RAM_BASE))?;
            f.write_all(data)?;
            f.flush()?;
            return Ok(());
        }
        self.pause()?;
        let mut g = Gdb::attach(self.gdb_port)?;
        g.write_mem(addr, data)?;
        self.cont()?;
        Ok(())
    }

    /// Read guest RAM while the VM is stopped.
    pub fn peek(&self, addr: u64, len: usize) -> Result<Vec<u8>> {
        if let Some(ramf) = &self.ram_file {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(ramf)?;
            f.seek(SeekFrom::Start(addr - RAM_BASE))?;
            let mut out = vec![0u8; len];
            f.read_exact(&mut out)?;
            return Ok(out);
        }
        self.pause()?;
        let mut g = Gdb::attach(self.gdb_port)?;
        let r = g.read_mem(addr, len);
        self.cont()?;
        r
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The spawned QEMU dies with the session — no orphaned emulators on error
/// paths or Ctrl-C.
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Helper used by qemu::spawn_session to pick a free TCP port (Windows path).
pub fn free_tcp_port() -> Result<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let p = l.local_addr()?.port();
    drop(l);
    Ok(p)
}

/// Command alias for control-plane users that need to build QEMU args the
/// same way the boot pipeline does (kept pub for the CLI integration tests).
pub fn qemu_binary() -> Result<PathBuf> {
    crate::qemu::find_qemu().ok_or_else(|| crate::Error::Msg("qemu-system-aarch64 not found".into()))
}

#[allow(dead_code)]
fn _silence(_: &Command, _: Stdio) {}
