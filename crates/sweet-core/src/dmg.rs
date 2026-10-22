//! UDIF disk-image reader for the lzfse-compressed rootfs DMG found in iOS
//! IPSWs (`087-*.dmg`, zip method 0, koly trailer + XML blkx resource).
//!
//! The image is a GPT disk whose Apple_APFS partition is stored as a table of
//! independently compressed chunks (blkx "mish" runs: zero / raw / lzfse /
//! zlib). We parse the chunk table once and expose random-access reads of the
//! *partition-relative* byte range, decoding chunks on demand through Apple's
//! vendored lzfse and miniz.

use crate::{bail, im4p, Result};
use std::collections::HashMap;

/// Random-access byte source (local file slice or remote range reader).
pub trait ByteSource {
    fn read_at(&mut self, off: u64, len: usize) -> Result<Vec<u8>>;
}

/// Reader for one zip-STORED IPSW member (local file or HTTP ranges via curl).
/// Remote reads go through a small LRU of 16 MiB windows: APFS/B-tree walks
/// touch 4 KiB blocks clustered inside ~1 MiB compressed chunks, so a window
/// turns hundreds of range requests into dozens.
pub struct MemberSource {
    source: String,
    data_off: u64,
    pub size: u64,
    windows: Vec<(u64, Vec<u8>)>, // (aligned start, bytes)
    window_size: u64,
    local: bool,
}

impl MemberSource {
    pub fn open(source: &str, member: &str) -> Result<Self> {
        let idx = crate::ipsw::index(source)?;
        let e = idx
            .entries
            .iter()
            .find(|e| e.name == member)
            .ok_or_else(|| crate::Error::Msg(format!("member {member:?} not in IPSW")))?;
        if e.method != 0 {
            bail!("member {member:?} is not zip-STORED (method {})", e.method);
        }
        // true local-header lengths (ZIP64 extras shift the data offset)
        let hdr = crate::ipsw::read_at(source, e.offset, 30)?;
        if u32le(&hdr, 0) != 0x0403_4b50 {
            bail!("bad local header for {member:?}");
        }
        let (nlen, elen) = (u16le(&hdr, 26) as u64, u16le(&hdr, 28) as u64);
        let local = !source.starts_with("http");
        let ws = if local { u64::MAX } else { 16 << 20 };
        Ok(Self {
            source: source.to_string(),
            data_off: e.offset + 30 + nlen + elen,
            size: e.size,
            windows: Vec::new(),
            window_size: ws,
            local,
        })
    }
}

impl ByteSource for MemberSource {
    fn read_at(&mut self, off: u64, len: usize) -> Result<Vec<u8>> {
        if self.local {
            return crate::ipsw::read_at(&self.source, self.data_off + off, len);
        }
        // windowed path (remote): align down, keep a small LRU of windows
        let wstart = off - (off % self.window_size);
        if let Some((s, ref w)) = self.windows.iter().find(|(s, _)| *s == wstart) {
            let s = (off - wstart) as usize;
            if s + len <= w.len() {
                return Ok(w[s..s + len].to_vec());
            }
        }
        // fetch the window clamped to member size
        let wlen = self.window_size.min(self.size.saturating_sub(wstart)) as usize;
        let w = crate::ipsw::read_at(&self.source, self.data_off + wstart, wlen)?;
        if self.windows.len() >= 4 {
            self.windows.remove(0);
        }
        self.windows.push((wstart, w.clone()));
        let s = (off - wstart) as usize;
        if s + len > w.len() {
            bail!("read past member end");
        }
        Ok(w[s..s + len].to_vec())
    }
}

/// Reader over a plain local file (the downloaded dmg member).
pub struct FileSource {
    f: std::fs::File,
}

impl FileSource {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self { f: std::fs::File::open(path)? })
    }
}

impl ByteSource for FileSource {
    fn read_at(&mut self, off: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        self.f.seek(SeekFrom::Start(off))?;
        let mut v = vec![0u8; len];
        self.f.read_exact(&mut v)?;
        Ok(v)
    }
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u32be(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64be(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]])
}

const CHUNK_ZERO: u32 = 0x0000_0000;
const CHUNK_RAW: u32 = 0x0000_0001;
const CHUNK_LZFSE: u32 = 0x8000_0007;
const CHUNK_ZLIB: u32 = 0x8000_0004;
const CHUNK_TERM: u32 = 0xFFFF_FFFF;

#[derive(Clone)]
struct Chunk {
    sector: u64,
    sectors: u64,
    kind: u32,
    coff: u64,
    clen: u64,
}

/// Random-access reader of the Apple_APFS partition inside a lzfse UDIF.
pub struct Dmg {
    src: Box<dyn ByteSource>,
    chunks: Vec<Chunk>,
    starts: Vec<u64>, // partition-relative byte offsets
    cache: HashMap<usize, Vec<u8>>,
    order: Vec<usize>,
    cache_max: usize,
    pub part_size: u64,
    stats_decoded: u64,
}

impl Dmg {
    /// Open from any byte source (the dmg file itself, incl. koly + XML).
    pub fn open(mut src: Box<dyn ByteSource>, size: u64) -> Result<Self> {
        if size < 512 {
            bail!("dmg too small");
        }
        let koly = src.read_at(size - 512, 512)?;
        if &koly[0..4] != b"koly" {
            bail!("no koly trailer — not a UDIF dmg");
        }
        let dfl = u64be(&koly, 0x18); // DataForkLength (big endian)
        // koly XML fields move between revisions — try the known spots and
        // validate, then fall back to scanning the tail region.
        let cands = [(u64be(&koly, 0xD4), u64be(&koly, 0xDC)), (u64be(&koly, 0xD8), u64be(&koly, 0xE0))];
        let mut xml: Option<Vec<u8>> = None;
        for (off, len) in cands {
            if off > 0 && len > 0 && off + len <= size && len < (16 << 20) {
                let v = src.read_at(off, len as usize)?;
                if v.starts_with(b"<?xml") {
                    xml = Some(v);
                    break;
                }
            }
        }
        let xml = match xml {
            Some(v) => v,
            None => {
                let start = dfl;
                if start + 6 >= size - 512 {
                    bail!("no xml region between data fork and koly");
                }
                let v = src.read_at(start, ((size - 512 - start) as usize).min(16 << 20))?;
                match v.windows(5).position(|w| w == b"<?xml") {
                    Some(p) => v[p..].to_vec(),
                    None => bail!("no <?xml plist found in dmg tail"),
                }
            }
        };
        let text = String::from_utf8_lossy(&xml).to_string();
        let mish = find_mish_block(&text, "Apple_APFS")
            .ok_or_else(|| crate::Error::Msg("Apple_APFS blkx not found in dmg plist".into()))?;

        // mish: magic(4) ver(4) sectorNumber u64be(8) sectorCount u64be(8) ...
        // runs at 0xCC, stride 0x28: type u32be, rsv u32be, sector u64be,
        // count u64be, coff u64be, clen u64be — all big endian.
        if &mish[0..4] != b"mish" {
            bail!("blkx Data is not a mish table");
        }
        let mut chunks = Vec::new();
        let n = (mish.len() - 0xCC) / 0x28;
        for k in 0..n {
            let o = 0xCC + k * 0x28;
            let kind = u32be(&mish, o);
            if kind == CHUNK_TERM {
                break;
            }
            let sector = u64be(&mish, o + 8);
            let sectors = u64be(&mish, o + 16);
            let coff = u64be(&mish, o + 24);
            let clen = u64be(&mish, o + 32);
            chunks.push(Chunk { sector, sectors, kind, coff, clen });
        }
        if chunks.is_empty() {
            bail!("Apple_APFS mish table has no runs");
        }
        let part_size = u64be(&mish, 16) * 512;
        let mut starts: Vec<u64> = chunks.iter().map(|c| c.sector * 512).collect();
        starts.sort_unstable();
        // rebuild chunks sorted by sector
        chunks.sort_by_key(|c| c.sector);
        let starts: Vec<u64> = chunks.iter().map(|c| c.sector * 512).collect();
        Ok(Self {
            src,
            chunks,
            starts,
            cache: HashMap::new(),
            order: Vec::new(),
            cache_max: 48,
            part_size,
            stats_decoded: 0,
        })
    }

    /// Partition-relative random read.
    pub fn read(&mut self, off: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut pos = off;
        let end = off + len as u64;
        while pos < end {
            let ci = match self.starts.binary_search(&pos) {
                Ok(i) => i,
                Err(0) => bail!("read before partition start ({off:#x})"),
                Err(i) => i - 1,
            };
            let (cstart, cbytes, coff, clen, kind) = {
                let c = &self.chunks[ci];
                (c.sector * 512, c.sectors * 512, c.coff, c.clen, c.kind)
            };
            if pos < cstart {
                bail!("gap in blkx table at {pos:#x}");
            }
            let data = self.chunk_data(ci, cbytes, coff, clen, kind)?;
            let within = (pos - cstart) as usize;
            let take = (data.len() - within).min((end - pos) as usize);
            if take == 0 {
                bail!("no progress reading chunk {ci} at {pos:#x}");
            }
            out.extend_from_slice(&data[within..within + take]);
            pos += take as u64;
        }
        Ok(out)
    }

    fn chunk_data(
        &mut self,
        ci: usize,
        cbytes: u64,
        coff: u64,
        clen: u64,
        kind: u32,
    ) -> Result<Vec<u8>> {
        if let Some(d) = self.cache.get(&ci) {
            return Ok(d.clone());
        }
        let data = match kind {
            CHUNK_ZERO => vec![0u8; cbytes as usize],
            CHUNK_RAW => self.src.read_at(coff, clen as usize)?,
            CHUNK_LZFSE => {
                let raw = self.src.read_at(coff, clen as usize)?;
                im4p::lzfse_exact(&raw, cbytes as usize)?
            }
            CHUNK_ZLIB => {
                let raw = self.src.read_at(coff, clen as usize)?;
                crate::ipsw::zlib_inflate(&raw, cbytes as usize)?
            }
            other => bail!("unsupported blkx chunk type {other:#x}"),
        };
        self.stats_decoded += 1;
        if self.cache.len() >= self.cache_max {
            let evict = self.order.remove(0);
            self.cache.remove(&evict);
        }
        self.cache.insert(ci, data.clone());
        self.order.push(ci);
        Ok(data)
    }
}

/// Find the base64 <data> payload of the blkx entry whose Name matches.
fn find_mish_block(plist: &str, name: &str) -> Option<Vec<u8>> {
    let mut search = 0usize;
    while let Some(dstart) = plist[search..].find("<data>") {
        let abs_d = search + dstart;
        let dend_rel = plist[abs_d..].find("</data>")?;
        let dend = abs_d + dend_rel;
        let b64: String = plist[abs_d + 6..dend]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let blob = match b64_decode(&b64) {
            Some(b) => b,
            None => {
                search = dend + 7;
                continue;
            }
        };
        if blob.len() > 4 && &blob[0..4] == b"mish" {
            // the <key>Name</key><string>NAME</string> pair follows Data
            // inside the same <dict>
            let tail_end = plist[dend..].find("</dict>").map(|i| dend + i).unwrap_or(plist.len());
            let tail = &plist[dend..tail_end];
            if let Some(k) = tail.find("<key>Name</key>") {
                if let Some(s) = tail[k..].find("<string>") {
                    let s_abs = k + s + 8;
                    let nm: String = tail[s_abs..]
                        .chars()
                        .take_while(|&c| c != '<')
                        .collect();
                    if nm.trim() == name {
                        return Some(blob);
                    }
                }
            }
        }
        search = dend + 7;
    }
    None
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
