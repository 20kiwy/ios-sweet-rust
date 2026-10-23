//! Minimal read-only APFS container/volume reader — just enough to walk the
//! sealed system volume of an iOS restore image and pull file bytes.
//!
//! Scope (validated against the real iPhone X 16.7.16 rootfs):
//! - NX superblock at container block 0 (restore images are freshly minted;
//!   checkpoint descent is only a fallback), container omap directly at
//!   nx_omap_oid (physical for this image class).
//! - volume list from nx_fs_oid[100], volume omap directly at apfs_omap_oid,
//!   fs tree root resolved through the volume omap (proper B-tree descent).
//! - catalog (fs) tree records: dir entries, inodes, file extents, xattrs.
//! - decmpfs transparent compression: algo 3/9/11 (attr) and 4/12 (rsrc),
//!   with resource-fork content stored as a dstream referenced by the
//!   com.apple.ResourceFork xattr value.

use crate::dmg::Dmg;
use crate::{bail, Result};
use std::collections::HashMap;

const OBJ_ID_MASK: u64 = 0x0FFF_FFFF_FFFF_FFFF;

const T_NXSB: u32 = 1;
const T_OMAP: u32 = 0x0B;
const T_CPMAP: u32 = 0x0C;

const BTNODE_ROOT: u16 = 0x0001;
const BTNODE_LEAF: u16 = 0x0002;
const BTNODE_FIXED: u16 = 0x0004;
const BTNODE_HASHED: u16 = 0x0008; // index values carry (child delta, sha256)

const NX_MAGIC: u32 = 0x4253_584E; // NXSB
const APSB_MAGIC: u32 = 0x4253_5041; // APSB

const J_INODE: u64 = 3;
const J_XATTR: u64 = 4;
const J_FILE_EXTENT: u64 = 8;
const J_DIR_REC: u64 = 9;

const XS_DECMPFS: &str = "com.apple.decmpfs";
const XS_RSRC: &str = "com.apple.ResourceFork";

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(v)
}

#[derive(Clone)]
struct Extent {
    laddr: u64,
    len: u64,
    pbn: u64,
    crypto: u64,
    flag: u64,
}

/// Fully indexed system volume: directories, inodes, extents, key xattrs.
pub struct Apfs {
    dmg: Dmg,
    bs: usize,
    /// parent dir id -> { name -> file id }
    pub dirs: HashMap<u64, HashMap<String, u64>>,
    /// id -> (mode, size)
    inodes: HashMap<u64, (u16, u64)>,
    /// id -> extents (files and dstreams)
    extents: HashMap<u64, Vec<Extent>>,
    /// id -> { xattr name -> raw j_xattr_val value }
    xattrs: HashMap<u64, HashMap<String, Vec<u8>>>,
}

impl Apfs {
    pub fn open(mut dmg: Dmg) -> Result<Self> {
        let b0 = dmg.read(0, 4096)?;
        if u32le(&b0, 0x20) != NX_MAGIC || u32le(&b0, 0x18) & 0xFFFF != T_NXSB {
            bail!("APFS container superblock not at block 0");
        }
        let bs = u32le(&b0, 0x24) as usize;
        if bs != 4096 {
            bail!("unsupported APFS block size {bs}");
        }
        let omap_oid = u64le(&b0, 0xA0);
        let mut ap = Self {
            dmg,
            bs,
            dirs: HashMap::new(),
            inodes: HashMap::new(),
            extents: HashMap::new(),
            xattrs: HashMap::new(),
        };

        // container omap -> volume superblock oids
        let om = ap.blk(omap_oid)?;
        if u32le(&om, 0x18) & 0xFFFF != T_OMAP {
            bail!("container omap oid {omap_oid:#x} is not an omap object");
        }
        let cot = u64le(&om, 0x30);

        let mut vol_oids = Vec::new();
        for i in 0..100usize {
            let v = u64le(&b0, 0xB8 + i * 8);
            if v != 0 {
                vol_oids.push(v);
            }
        }
        let mut fs_roots = Vec::new();
        for voi in vol_oids {
            let apsb_p = match ap.omap_lookup(cot, voi) {
                Ok(p) => p,
                Err(_) => continue, // nx_xp_data holds checkpoint/mapping oids too
            };
            let v = ap.blk(apsb_p)?;
            if u32le(&v, 0x20) != APSB_MAGIC {
                continue; // not a volume superblock
            }
            let vol_omap_oid = u64le(&v, 0x80);
            let root_tree_oid = u64le(&v, 0x88);
            fs_roots.push((vol_omap_oid, root_tree_oid));
        }
        // Fallback: walk the container omap B-tree leaves and resolve every
        // mapping; volumes are whatever validates as APSB. Checkpoint chains
        // carry STALE superblocks (older xid) whose root-tree oids may no
        // longer exist — candidates are tried in turn below.
        let mut seen = std::collections::HashSet::new();
        for (_oid, _xid, paddr) in ap.collect_omap_entries(cot)? {
            if !seen.insert(paddr) {
                continue;
            }
            let v = ap.blk(paddr)?;
            if u32le(&v, 0x20) != APSB_MAGIC {
                continue;
            }
            let vol_omap_oid = u64le(&v, 0x80);
            let root_tree_oid = u64le(&v, 0x88);
            fs_roots.push((vol_omap_oid, root_tree_oid));
        }
        if fs_roots.is_empty() {
            bail!("no APFS volumes found");
        }
        // Index every volume's catalog tree; tolerate stale checkpoint
        // candidates as long as one volume indexes.
        let mut indexed = 0usize;
        let mut last_err = None;
        if std::env::var("SWEET_APFS_DEBUG").is_ok() {
            eprintln!("apfs: {} candidates", fs_roots.len());
            for (vo, rt) in &fs_roots { eprintln!("  cand: vol-omap={vo:#x} root-tree={rt:#x}"); }
        }
        for (vol_omap_oid, root_tree_oid) in fs_roots {
            let vom = match ap.blk(vol_omap_oid) {
                Ok(v) if u32le(&v, 0x18) & 0xFFFF == T_OMAP => v,
                _ => continue, // stale checkpoint: not a live omap
            };
            let vot = u64le(&vom, 0x30);
            if std::env::var("SWEET_APFS_DEBUG").is_ok() {
                eprintln!("  try: vol-omap={vol_omap_oid:#x} vot={vot:#x} root={root_tree_oid:#x}");
            }
            let root = match ap.omap_lookup(vot, root_tree_oid) {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e);
                    continue; // stale checkpoint: root tree gone
                }
            };
            match ap.index_tree(root, vot, root_tree_oid) {
                Ok(()) => indexed += 1,
                Err(e) => last_err = Some(e),
            }
        }
        if indexed == 0 {
            bail!(
                "no APFS volume indexed: {}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            );
        }
        Ok(ap)
    }

    fn blk(&mut self, paddr: u64) -> Result<Vec<u8>> {
        self.dmg.read(paddr * self.bs as u64, self.bs)
    }

    /// Resolve a virtual oid through an omap B-tree (physical root).
    fn omap_lookup(&mut self, tree_root: u64, oid: u64) -> Result<u64> {
        let mut node_p = tree_root;
        loop {
            let b = self.blk(node_p)?;
            let (flags, level, nkeys) = parse_node_hdr(&b);
            let ents = node_entries(&b, flags, nkeys, self.bs)?;
            if level > 0 {
                let mut best: Option<usize> = None;
                for (i, e) in ents.iter().enumerate() {
                    let koid = u64le(&b, e.0);
                    if koid <= oid {
                        best = Some(i);
                    }
                }
                let i = best.ok_or_else(|| {
                    crate::Error::Msg(format!("omap descend: oid {oid:#x} not found"))
                })?;
                node_p = u64le(&b, ents[i].2);
                continue;
            }
            let mut best: Option<(u64, (usize, usize))> = None;
            for e in ents.iter() {
                let koid = u64le(&b, e.0);
                let kxid = u64le(&b, e.0 + 8);
                if koid == oid && best.map(|(x, _)| kxid >= x).unwrap_or(true) {
                    best = Some((kxid, (e.2, e.3)));
                }
            }
            let (_, (val_off, _)) = best
                .ok_or_else(|| crate::Error::Msg(format!("omap leaf: oid {oid:#x} not found")))?;
            return Ok(u64le(&b, val_off + 8)); // omap_val { flags u32, size u32, paddr u64 }
        }
    }

    /// Every (oid, xid, paddr) mapping in an omap B-tree (leaf pass).
    fn collect_omap_entries(&mut self, tree_root: u64) -> Result<Vec<(u64, u64, u64)>> {
        let mut out = Vec::new();
        let mut stack = vec![tree_root];
        while let Some(node_p) = stack.pop() {
            let b = self.blk(node_p)?;
            let (flags, level, nkeys) = parse_node_hdr(&b);
            let ents = node_entries(&b, flags, nkeys, self.bs)?;
            if level > 0 {
                for e in ents.iter() {
                    stack.push(u64le(&b, e.2));
                }
                continue;
            }
            for e in ents.iter() {
                let oid = u64le(&b, e.0);
                let xid = u64le(&b, e.0 + 8);
                let paddr = u64le(&b, e.2 + 8);
                out.push((oid, xid, paddr));
            }
        }
        Ok(out)
    }

    /// One full pass over a catalog tree collecting dirs/inodes/extents/xattrs.
    fn index_tree(&mut self, root_paddr: u64, omap_root: u64, root_oid: u64) -> Result<()> {
        // iterative DFS; children are virtual oids resolved via the volume
        // omap. SEALED volumes: fs-tree nodes are BTNODE_HASHED (flag 0x8) —
        // index values are (child_delta u64, 32-byte sha256); the real child
        // oid is delta + root_oid (apfs-fuse: binv_child_oid + m_oid).
        let mut stack = vec![root_paddr];
        while let Some(p) = stack.pop() {
            let b = self.blk(p)?;
            let (flags, level, nkeys) = parse_node_hdr(&b);
            let ents = node_entries(&b, flags, nkeys, self.bs)?;
            if level > 0 {
                let hashed = flags & BTNODE_HASHED != 0;
                for e in ents.iter().rev() {
                    let mut child = u64le(&b, e.2);
                    if hashed {
                        child = child.wrapping_add(root_oid);
                    }
                    let cp = self.omap_lookup(omap_root, child)?;
                    stack.push(cp);
                }
                continue;
            }
            for e in ents.iter() {
                let k64 = u64le(&b, e.0);
                let kid = k64 & OBJ_ID_MASK;
                let ktype = k64 >> 60;
                match ktype {
                    J_DIR_REC => {
                        let nlh = u32le(&b, e.0 + 8);
                        let nlen = (nlh & 0x3FF) as usize;
                        let name = String::from_utf8_lossy(
                            &b[e.0 + 12..(e.0 + 12 + nlen).min(b.len())],
                        )
                        .split('\0')
                        .next()
                        .unwrap_or("")
                        .to_string();
                        let file_id = u64le(&b, e.2);
                        self.dirs.entry(kid).or_default().insert(name, file_id);
                    }
                    J_INODE => {
                        if e.3 >= 92 {
                            let mode = u16le(&b, e.2 + 80);
                            let size = u64le(&b, e.2 + 84);
                            self.inodes.insert(kid, (mode, size));
                        }
                    }
                    J_FILE_EXTENT => {
                        let laddr = u64le(&b, e.0 + 8);
                        let len_flags = u64le(&b, e.2);
                        let pbn = u64le(&b, e.2 + 8);
                        let crypto = u64le(&b, e.2 + 16);
                        self.extents.entry(kid).or_default().push(Extent {
                            laddr,
                            len: len_flags & 0x00FF_FFFF_FFFF_FFFF,
                            pbn,
                            crypto,
                            flag: len_flags >> 56,
                        });
                    }
                    J_XATTR => {
                        let nl = u16le(&b, e.0 + 8) as usize;
                        let name = String::from_utf8_lossy(
                            &b[e.0 + 10..(e.0 + 10 + nl).min(b.len())],
                        )
                        .split('\0')
                        .next()
                        .unwrap_or("")
                        .to_string();
                        if name == XS_DECMPFS || name == XS_RSRC {
                            let val = b[e.2..(e.2 + e.3).min(b.len())].to_vec();
                            self.xattrs.entry(kid).or_default().insert(name, val);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Resolve an absolute path (starting at the volume root, id 2).
    pub fn resolve(&self, path: &str) -> Result<u64> {
        let mut cur = 2u64;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let next = self
                .dirs
                .get(&cur)
                .and_then(|m| m.get(part))
                .copied()
                .ok_or_else(|| crate::Error::Msg(format!("path not found: {path}")))?;
            cur = next;
        }
        Ok(cur)
    }

    /// Sorted directory listing: (name, fid, is_dir).
    pub fn list_dir(&self, dir: u64) -> Vec<(String, u64, bool)> {
        let mut v: Vec<(String, u64, bool)> = self
            .dirs
            .get(&dir)
            .map(|m| {
                m.iter()
                    .map(|(n, f)| (n.clone(), *f, self.dirs.contains_key(f)))
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Every entry in the volume as (full path, fid), sorted by path.
    pub fn walk_all(&self) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = vec![("/".into(), 2)];
        let mut stack = vec![(String::new(), 2u64)];
        while let Some((prefix, dir)) = stack.pop() {
            if let Some(m) = self.dirs.get(&dir) {
                for (name, fid) in m {
                    let path = format!("{prefix}/{name}");
                    if self.dirs.contains_key(fid) {
                        stack.push((path.clone(), *fid));
                    }
                    out.push((path, *fid));
                }
            }
        }
        out.sort();
        out
    }

    /// (mode, size) for a file id, if indexed. Size prefers real data
    /// coverage (extents / decmpfs) over the catalog inode field, which is
    /// 0 for files whose inode_val omits the optional uncompressed-size
    /// field (observed on WallpaperKit HEIC assets in 16.7.16).
    pub fn stat(&self, fid: u64) -> Option<(u16, u64)> {
        let size = self.file_size(fid);
        let mode = self.inodes.get(&fid).map(|(m, _)| *m)?;
        Some((mode, size))
    }

    /// Best-effort logical file size: extent coverage -> decmpfs record ->
    /// inode size field.
    fn file_size(&self, fid: u64) -> u64 {
        if let Some(exts) = self.extents.get(&fid) {
            let m = exts.iter().map(|e| e.laddr + e.len).max().unwrap_or(0);
            if m > 0 {
                return m;
            }
        }
        if let Some(dec) = self.xattrs.get(&fid).and_then(|m| m.get(XS_DECMPFS)) {
            if dec.len() >= 20 {
                return u64le(dec, 12); // cmpf header: u16 flags, u16 len, magic, algo u32, usize u64
            }
        }
        self.inodes.get(&fid).map(|(_, s)| *s).unwrap_or(0)
    }

    /// decmpfs raw record for a file id (for provenance diagnostics).
    pub fn xattr(&self, fid: u64, name: &str) -> Option<&Vec<u8>> {
        self.xattrs.get(&fid)?.get(name)
    }

    /// Full file bytes, transparently decompressing decmpfs content.
    pub fn read_file(&mut self, fid: u64) -> Result<Vec<u8>> {
        let (mode, size) = (
            self.inodes.get(&fid).map(|(m, _)| *m).unwrap_or(0),
            self.file_size(fid),
        );
        let _ = mode;
        if size == 0 {
            bail!("no data for inode {fid}");
        }
        if let Some(exts) = self.extents.get(&fid).cloned() {
            return self.read_extents(&exts, size);
        }
        // transparent compression via decmpfs (+ resource fork dstream)
        let dec = self
            .xattrs
            .get(&fid)
            .and_then(|m| m.get(XS_DECMPFS))
            .cloned()
            .ok_or_else(|| crate::Error::Msg(format!("no data for inode {fid}")))?;
        let rsrc = self.xattrs.get(&fid).and_then(|m| m.get(XS_RSRC)).cloned();
        let (xflags, xlen) = (u16le(&dec, 0), u16le(&dec, 2) as usize);
        let payload = &dec[4..(4 + xlen).min(dec.len())];
        if payload.len() < 16 || &payload[0..4] != b"fpmc" {
            bail!("bad cmpf header");
        }
        let algo = u32le(payload, 4);
        let usize_ = u64le(payload, 8);
        let _ = xflags;
        let rsrc_data = match rsrc {
            Some(rf) => {
                let (rflags, rlen) = (u16le(&rf, 0), u16le(&rf, 2) as usize);
                let xdata = &rf[4..(4 + rlen).min(rf.len())];
                if xdata.len() == 48 {
                    // j_xattr_dstream_t { obj_id u64, dstream { size u64, .. } }
                    let oid = u64le(xdata, 0);
                    let dsize = u64le(xdata, 8);
                    let exts = self
                        .extents
                        .get(&oid)
                        .cloned()
                        .ok_or_else(|| crate::Error::Msg("rsrc dstream has no extents".into()))?;
                    Some(self.read_extents(&exts, dsize)?)
                } else {
                    Some(xdata.to_vec())
                }
            }
            None => None,
        };
        decompress(algo, usize_ as usize, &payload[16..], rsrc_data.as_deref())
    }

    fn read_extents(&mut self, exts: &[Extent], size: u64) -> Result<Vec<u8>> {
        let mut out = vec![0u8; size as usize];
        let bs = self.bs as u64;
        let mut filled = 0usize;
        for e in sorted_extents(exts) {
            if e.crypto != 0 {
                bail!("encrypted extent (not supported)");
            }
            if e.flag & 0x80 != 0 {
                bail!("PEXT extents not supported");
            }
            let start = e.pbn * bs + (e.laddr % bs);
            let want = e.len as usize;
            let got = self.dmg.read(start, want + (e.laddr % bs) as usize)?;
            let slice = &got[(e.laddr % bs) as usize..];
            let n = want.min(out.len() - filled).min(slice.len());
            out[filled..filled + n].copy_from_slice(&slice[..n]);
            filled += n;
            if filled >= out.len() {
                break;
            }
        }
        Ok(out)
    }
}

fn sorted_extents(exts: &[Extent]) -> Vec<&Extent> {
    let mut v: Vec<&Extent> = exts.iter().collect();
    v.sort_by_key(|e| e.laddr);
    v
}

fn parse_node_hdr(b: &[u8]) -> (u16, u16, u32) {
    let flags = u16le(b, 0x20);
    let level = u16le(b, 0x22);
    let nkeys = u32le(b, 0x24);
    (flags, level, nkeys)
}

/// (key_off, key_len, val_off, val_len) — offsets absolute in the node block.
type Ent = (usize, usize, usize, usize);

fn node_entries(b: &[u8], flags: u16, nkeys: u32, bs: usize) -> Result<Vec<Ent>> {
    let toc_off = u16le(b, 0x28) as usize; // always 0 in practice
    let toc_len = u16le(b, 0x2A) as usize;
    let _ = toc_off;
    let keys_start = 56 + toc_len;
    let vals_start = bs - if flags & BTNODE_ROOT != 0 { 40 } else { 0 };
    let fixed = flags & BTNODE_FIXED != 0;
    let mut out = Vec::with_capacity(nkeys as usize);
    for i in 0..nkeys as usize {
        if fixed {
            // fixed-size kv: toc entries are (key_off u16, val_off u16)
            let (k, v) = (u16le(b, 56 + i * 4), u16le(b, 56 + i * 4 + 2));
            if v == 0xFFFF {
                continue;
            }
            out.push((keys_start + k as usize, 0, vals_start - v as usize, 0));
        } else {
            let o = 56 + i * 8;
            let (ko, kl, vo, vl) = (
                u16le(b, o) as usize,
                u16le(b, o + 2) as usize,
                u16le(b, o + 4) as usize,
                u16le(b, o + 6) as usize,
            );
            if vo == 0xFFFF {
                continue;
            }
            out.push((keys_start + ko, kl, vals_start - vo, vl));
        }
    }
    Ok(out)
}

/// decmpfs decompression (Apple transparent compression).
fn decompress(algo: u32, usize_: usize, inline: &[u8], rsrc: Option<&[u8]>) -> Result<Vec<u8>> {
    match algo {
        3 => crate::ipsw::zlib_inflate(inline, usize_),
        9 => Ok(inline[..usize_.min(inline.len())].to_vec()),
        11 => crate::im4p::lzfse_exact(inline, usize_),
        4 | 12 => {
            let rsrc = rsrc.ok_or_else(|| crate::Error::Msg("algo needs resource fork".into()))?;
            if algo == 4 {
                // RsrcForkHeader (BE) + CmpfRsrc { entries u32le, [off,size] u32le }
                let data_offset = be32(rsrc, 0);
                let base = data_offset as usize + 4;
                let entries = u32le(rsrc, base) as usize;
                let mut out = vec![0u8; (usize_ + 0xFFFF) & !0xFFFF];
                for k in 0..entries {
                    let (off, ln) = {
                        let o = base + 4 + k * 8;
                        (u32le(rsrc, o) as usize, u32le(rsrc, o + 4) as usize)
                    };
                    let src = &rsrc[base + off..(base + off + ln).min(rsrc.len())];
                    let dst = &mut out[k * 0x10000..(k + 1) * 0x10000];
                    if src[0] == 0x78 {
                        crate::ipsw::inflate_into(src, dst)?;
                    } else if src[0] & 0x0F == 0x0F {
                        dst[..src.len() - 1].copy_from_slice(&src[1..]);
                    } else {
                        bail!("bad zlib chunk {:#x}", src[0]);
                    }
                }
                out.truncate(usize_);
                Ok(out)
            } else {
                // algo 12: u32 offset list, lzfse 64K chunks
                if rsrc.len() < 8 {
                    bail!("rsrc too small for offset list");
                }
                let n = rsrc.len() / 4;
                let mut off_list = Vec::with_capacity(n);
                for i in 0..n {
                    off_list.push(u32le(rsrc, i * 4) as usize);
                }
                let mut out = vec![0u8; (usize_ + 0xFFFF) & !0xFFFF];
                let lz = LibLzfseExact;
                let mut k = 0usize;
                while (k << 16) < out.len() {
                    let (s, e) = (off_list[k], off_list[k + 1]);
                    let chunk = lz.decode(&rsrc[s..e], 0x10000)?;
                    let dst = &mut out[k << 16..(k << 16) + chunk.len().min(0x10000)];
                    dst.copy_from_slice(&chunk[..dst.len()]);
                    k += 1;
                }
                out.truncate(usize_);
                Ok(out)
            }
        }
        a => bail!("decmpfs algo {a} not supported"),
    }
}

struct LibLzfseExact;
impl LibLzfseExact {
    fn decode(&self, src: &[u8], cap: usize) -> Result<Vec<u8>> {
        let mut dst = vec![0u8; cap + 64];
        let n = unsafe {
            lzfse_raw(
                dst.as_mut_ptr(),
                dst.len(),
                src.as_ptr(),
                src.len(),
                std::ptr::null_mut(),
            )
        };
        if n == 0 {
            bail!("lzfse rsrc chunk failed");
        }
        dst.truncate(n);
        Ok(dst)
    }
}

extern "C" {
    #[link_name = "lzfse_decode_buffer"]
    fn lzfse_raw(dst: *mut u8, dst_size: usize, src: *const u8, src_size: usize, scratch: *mut u8) -> usize;
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
