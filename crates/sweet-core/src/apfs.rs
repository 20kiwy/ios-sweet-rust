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
use crate::error::Error;
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
    inodes: HashMap<u64, (u16, u32, u32, u64)>,
    /// inode_val raw value length (diagnostics: optional-field presence)
    inode_val_lens: HashMap<u64, u64>,
    /// raw inode_val bytes (SWEET_INODE_DUMP diagnostics only)
    inode_raw: HashMap<u64, Vec<u8>>,
    /// catalog kid -> inode private_id (fext tree key)
    priv_ids: HashMap<u64, u64>,
    /// SEALED volumes: file extents live in the fext tree
    /// (private_id -> extents); the catalog has only data-hash records.
    fext: HashMap<u64, Vec<Extent>>,
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
            inode_val_lens: HashMap::new(),
            inode_raw: HashMap::new(),
            priv_ids: HashMap::new(),
            fext: HashMap::new(),
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
        let mut fs_roots: Vec<(u64, u64, u64)> = Vec::new(); // (omap, root, apsb_paddr)
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
            fs_roots.push((vol_omap_oid, root_tree_oid, apsb_p));
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
            fs_roots.push((vol_omap_oid, root_tree_oid, paddr));
        }
        if fs_roots.is_empty() {
            bail!("no APFS volumes found");
        }
        // Index every volume's catalog tree; tolerate stale checkpoint
        // candidates as long as one volume indexes.
        let mut indexed = 0usize;
        let mut last_err = None;
        let mut live_apsb: Option<u64> = None;
        let mut tried: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
        for (vol_omap_oid, root_tree_oid, apsb_p) in fs_roots {
            if !tried.insert((vol_omap_oid, root_tree_oid)) {
                continue; // same volume enumerated twice — indexing it twice
                          // would duplicate every extent record
            }
            let vom = match ap.blk(vol_omap_oid) {
                Ok(v) if u32le(&v, 0x18) & 0xFFFF == T_OMAP => v,
                _ => continue, // stale checkpoint: not a live omap
            };
            let vot = u64le(&vom, 0x30);
            let root = match ap.omap_lookup(vot, root_tree_oid) {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e);
                    continue; // stale checkpoint: root tree gone
                }
            };
            match ap.index_tree(root, vot, root_tree_oid) {
                Ok(()) => {
                    indexed += 1;
                    // remember the live volume superblock (fext tree oid)
                    live_apsb = Some(apsb_p);
                }
                Err(e) => last_err = Some(e),
            }
        }

        if indexed == 0 {
            bail!(
                "no APFS volume indexed: {}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            );
        }
        // SEALED volumes: file extents live in the fext tree (physical root,
        // fixed keys (private_id, laddr) -> (len_and_flags, paddr)).
        if let Some(apsb_p) = live_apsb {
            let apsb = ap.blk(apsb_p)?;
            let fext_oid = u64le(&apsb, 0x408);
            if fext_oid != 0 {
                match ap.index_fext_tree(fext_oid) {
                    Ok(n) => {
                        if n > 0 {
                            eprintln!("[apfs] sealed volume: {} fext extents", n);
                        }
                    }
                    Err(e) => last_err = Some(e),
                }
            }
        }
        Ok(ap)
    }

    /// Index the sealed-volume fext tree (physical root, FIXED keys).
    fn index_fext_tree(&mut self, root_paddr: u64) -> Result<usize> {
        let mut n = 0usize;
        let mut stack = vec![root_paddr];
        let mut seen = std::collections::HashSet::new();
        while let Some(p) = stack.pop() {
            if !seen.insert(p) {
                continue;
            }
            let b = self.blk(p)?;
            let (flags, level, nkeys) = parse_node_hdr(&b);
            let ents = node_entries(&b, flags, nkeys, self.bs)?;
            if level > 0 {
                for e in ents.iter() {
                    stack.push(u64le(&b, e.2));
                }
                continue;
            }
            for e in ents.iter() {
                let priv_id = u64le(&b, e.0) & OBJ_ID_MASK;
                let _laddr = u64le(&b, e.0 + 8);
                let len_flags = u64le(&b, e.2);
                let pbn = u64le(&b, e.2 + 8);
                let ext = Extent {
                    laddr: _laddr,
                    len: len_flags & 0x00FF_FFFF_FFFF_FFFF,
                    pbn,
                    crypto: 0,
                    flag: len_flags >> 56,
                };
                let list = self.fext.entry(priv_id).or_default();
                if !list
                    .iter()
                    .any(|x| x.laddr == ext.laddr && x.pbn == ext.pbn && x.len == ext.len)
                {
                    list.push(ext);
                    n += 1;
                }
            }
        }
        for list in self.fext.values_mut() {
            list.sort_by_key(|e| e.laddr);
        }
        Ok(n)
    }

    /// Extents for a file id: catalog records first, then the sealed-volume
    /// fext tree (keyed by the inode's private_id).
    pub fn extents_for(&self, fid: u64) -> Option<Vec<Extent>> {
        if let Some(v) = self.extents.get(&fid) {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
        // xattr dstream files have no inode: their oid IS the fext key
        if let Some(v) = self.fext.get(&fid) {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
        let priv_id = self.priv_ids.get(&fid).copied()?;
        self.fext.get(&priv_id).cloned()
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
                        // apfs_inode_val: owner u32 @72, group u32 @76,
                        // mode u16 @80, pad u16 @82, optional
                        // uncompressed_size u64 @84 (present iff val >= 92)
                        let mode = if e.3 >= 82 { u16le(&b, e.2 + 80) } else { 0 };
                        let owner = if e.3 >= 76 { u32le(&b, e.2 + 72) } else { 0 };
                        let group = if e.3 >= 80 { u32le(&b, e.2 + 76) } else { 0 };
                        let size = if e.3 >= 92 { u64le(&b, e.2 + 84) } else { 0 };
                        self.inode_val_lens.insert(kid, e.3 as u64);
                        // keep raw inode_val: xfield chains (hardlink sizes)
                        // are needed for exact file sizes at build time
                        let end = (e.2 + e.3).min(b.len());
                        self.inode_raw.insert(kid, b[e.2..end].to_vec());
                        self.inodes.insert(kid, (mode, owner, group, size));
                        // private_id (hardlink-shared stream id; fext key)
                        let priv_id = u64le(&b, e.2 + 8) & OBJ_ID_MASK;
                        self.priv_ids.insert(kid, priv_id);
                    }
                    J_FILE_EXTENT => {
                        let laddr = u64le(&b, e.0 + 8);
                        let len_flags = u64le(&b, e.2);
                        let pbn = u64le(&b, e.2 + 8);
                        let crypto = u64le(&b, e.2 + 16);
                        let ext = Extent {
                            laddr,
                            len: len_flags & 0x00FF_FFFF_FFFF_FFFF,
                            pbn,
                            crypto,
                            flag: len_flags >> 56,
                        };
                        let list = self.extents.entry(kid).or_default();
                        // guard against duplicate catalog visits
                        if !list.iter().any(|x| {
                            x.laddr == ext.laddr && x.pbn == ext.pbn && x.len == ext.len
                        }) {
                            list.push(ext);
                        }
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
                        // capture every xattr (decmpfs sizes, resource
                        // forks, com.apple.fs.symlink targets, ...)
                        let val = b[e.2..(e.2 + e.3).min(b.len())].to_vec();
                        self.xattrs.entry(kid).or_default().insert(name, val);
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

    /// Directory test by inode mode (S_IFDIR). The "has children" heuristic
    /// misclassifies EMPTY directories — on the real rootfs /dev and /cores
    /// are empty dirs; /dev is the XNU devfs_kernel_mount mountpoint
    /// (bsd_init.c), without which launchd dies with
    /// "failed to reserve stdin descriptor: /dev/null: 2".
    fn is_dir_fid(&self, fid: u64) -> bool {
        if let Some(&(mode, ..)) = self.inodes.get(&fid) {
            return (mode & 0o170000) == 0o040000;
        }
        // no inode record: fall back to the children heuristic
        self.dirs.contains_key(&fid)
    }

    /// Sorted directory listing: (name, fid, is_dir).
    pub fn list_dir(&self, dir: u64) -> Vec<(String, u64, bool)> {
        let mut v: Vec<(String, u64, bool)> = self
            .dirs
            .get(&dir)
            .map(|m| {
                m.iter()
                    .map(|(n, f)| (n.clone(), *f, self.is_dir_fid(*f)))
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
    /// Extent table (paddr, len) for a file id (diagnostics + mkiosroot).
    pub fn extent_list(&self, fid: u64) -> Vec<(u64, u64, u64)> {
        self.extents_for(fid)
            .map(|v| v.iter().map(|e| (e.laddr, e.pbn, e.len)).collect())
            .unwrap_or_default()
    }

    /// All xattr names for a file id.
    pub fn xattr_names(&self, fid: u64) -> Vec<String> {
        self.xattrs
            .get(&fid)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Resolve an xattr's CONTENT: inline values come straight from the
    /// catalog record; data-stream values (huge rsrc forks) reference a
    /// separate file (oid + size in the dstream) whose extents live in the
    /// same catalog.
    pub fn xattr_resolved(&mut self, fid: u64, name: &str) -> Result<Option<Vec<u8>>> {
        const XATTR_DATA_STREAM: u16 = 0x1;
        let Some(raw) = self.xattrs.get(&fid).and_then(|m| m.get(name)).cloned() else {
            return Ok(None);
        };
        if raw.len() < 4 {
            return Ok(Some(raw));
        }
        let xflags = u16le(&raw, 0);
        let xdata_len = u16le(&raw, 2) as usize;
        if xflags & XATTR_DATA_STREAM == 0 {
            return Ok(Some(raw[4..(4 + xdata_len).min(raw.len())].to_vec()));
        }
        let d = &raw[4..(4 + xdata_len).min(raw.len())];
        if d.len() < 16 {
            bail!("sized xattr {name}: dstream too small");
        }
        let oid = u64le(d, 0) & OBJ_ID_MASK;
        let size = u64le(d, 8);
        let exts = self
            .extents_for(oid)
            .ok_or_else(|| Error::Msg(format!("sized xattr {name}: no extents for oid {oid:#x}")))?;
        let covered: u64 = exts.iter().map(|e| e.len).sum();
        let data = self.read_extents(&exts, size)?;
        if data.len() as u64 != size {
            bail!(
                "sized xattr {name}: got {} bytes, want {size} (exts={} covered={covered})",
                data.len(),
                exts.len()
            );
        }
        Ok(Some(data))
    }

    /// Raw inode_val length for a file id (diagnostics).
    pub fn inode_val_len(&self, fid: u64) -> Option<u64> {
        self.inode_val_lens.get(&fid).copied()
    }

    /// Raw inode_val bytes (requires SWEET_INODE_DUMP=1 at open).
    pub fn inode_raw(&self, fid: u64) -> Option<&[u8]> {
        self.inode_raw.get(&fid).map(|v| v.as_slice())
    }

    /// (mode, uid, gid, size) for a file id, if indexed. Size prefers real
    /// data coverage (extents / decmpfs) over the catalog inode field, which
    /// is 0 for files whose inode_val omits the optional uncompressed-size
    /// field (observed on WallpaperKit HEIC assets in 16.7.16).
    pub fn stat(&self, fid: u64) -> Option<(u16, u32, u32, u64)> {
        let size = self.file_size(fid);
        let (mode, uid, gid) = self.inodes.get(&fid).map(|(m, o, g, _)| (*m, *o, *g))?;
        Some((mode, uid, gid, size))
    }

    /// Exact logical file size. Order matters: decmpfs record (exact) ->
    /// inode uncompressed_size at +84 (present only in 92-byte inode_vals) ->
    /// xfield hardlink-info size (longer inode_vals) -> extent coverage
    /// (block-rounded LAST-RESORT — a rounded size corrupts binary-plist
    /// trailers, so it is only used when nothing exact exists).
    fn file_size(&self, fid: u64) -> u64 {
        if let Some(dec) = self.xattrs.get(&fid).and_then(|m| m.get(XS_DECMPFS)) {
            if dec.len() >= 20 {
                return u64le(dec, 12); // cmpf header: u16 flags, u16 len, magic, algo u32, usize u64
            }
        }
        if let Some((_, _, _, size, vlen)) = self.inode_full(fid) {
            if vlen == 92 {
                // plain inode: uncompressed_size at +84 is exact
                if size > 0 {
                    return size;
                }
            }
            if vlen > 92 {
                // xfield inode: the dstream xfield carries the exact size
                if let Some(sz) = self.inode_dstream_size(fid) {
                    return sz;
                }
            }
        }
        if let Some(exts) = self.extents.get(&fid) {
            let m = exts.iter().map(|e| e.laddr + e.len).max().unwrap_or(0);
            if m > 0 {
                return m;
            }
        }
        0
    }

    fn inode_full(&self, fid: u64) -> Option<(u16, u32, u32, u64, u64)> {
        let (m, o, g, s) = self.inodes.get(&fid).copied()?;
        let vlen = self.inode_val_lens.get(&fid).copied()?;
        Some((m, o, g, s, vlen))
    }

    /// Parse the xfield blob at the end of a long inode_val:
    /// xf_blob { u16 num_exts, u16 used_data } then ALL descriptors
    /// (u8 type, u8 flags, u16 size), then the values (each padded to 8).
    /// Returns (type, flags, value) triples.
    pub fn inode_xfields(&self, fid: u64) -> Option<Vec<(u8, u8, Vec<u8>)>> {
        let raw = self.inode_raw.get(&fid)?;
        if raw.len() < 96 {
            return None;
        }
        let num = u16le(raw, 92) as usize;
        let mut descs = Vec::with_capacity(num);
        for i in 0..num {
            let o = 96 + i * 4;
            if o + 4 > raw.len() {
                return None;
            }
            descs.push((raw[o], raw[o + 1], u16le(raw, o + 2) as usize));
        }
        let mut vo = 96 + num * 4;
        let mut out = Vec::with_capacity(num);
        for (t, f, sz) in descs {
            if vo + sz > raw.len() {
                return None;
            }
            out.push((t, f, raw[vo..vo + sz].to_vec()));
            vo += (sz + 7) & !7;
        }
        Some(out)
    }

    /// Logical size from the DSTREAM xfield (type 8): first u64 = file size.
    fn inode_dstream_size(&self, fid: u64) -> Option<u64> {
        for (t, _, d) in self.inode_xfields(fid)? {
            if t == 8 && d.len() >= 8 {
                return Some(u64le(&d, 0));
            }
        }
        None
    }

    /// Stream a file's logical bytes to `sink` (16 MiB chunks). decmpfs
    /// files are decompressed whole; plain files stream extent-by-extent.
    /// Returns the bytes produced.
    pub fn copy_file(&mut self, fid: u64, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<u64> {
        let size = self.file_size(fid);
        if size == 0 {
            return Ok(0);
        }
        if self.xattrs.get(&fid).and_then(|m| m.get(XS_DECMPFS)).is_some() {
            let whole = self.read_file(fid)?;
            sink(&whole)?;
            return Ok(whole.len() as u64);
        }
        if let Some(exts) = self.extents_for(fid) {
            let mut left = size;
            for e in &exts {
                if left == 0 {
                    break;
                }
                let mut off = 0u64;
                while off < e.len && left > 0 {
                    let take = (e.len - off).min(left).min(16 << 20);
                    let b = self.dmg.read(
                        (e.pbn * self.bs as u64) + off,
                        take as usize,
                    )?;
                    sink(&b)?;
                    off += take;
                    left -= take;
                }
            }
            if left > 0 {
                bail!("copy_file: extents short by {left}");
            }
            return Ok(size);
        }
        bail!("copy_file: no extents for inode {fid}");
    }

    /// Read [off, off+len) of a PLAIN file's logical bytes into out.
    /// (Compressed files must go through read_file.)
    pub fn read_range(&mut self, fid: u64, off: u64, out: &mut [u8]) -> Result<usize> {
        if self.xattrs.get(&fid).and_then(|m| m.get(XS_DECMPFS)).is_some() {
            bail!("read_range: inode {fid} is decmpfs-compressed");
        }
        let exts = self
            .extents_for(fid)
            .ok_or_else(|| Error::Msg(format!("read_range: no extents for {fid}")))?;
        let mut filled = 0usize;
        let mut pos = off;
        let want = out.len();
        for e in &exts {
            if filled == want {
                break;
            }
            let e_end = e.laddr + e.len;
            if pos >= e_end {
                continue;
            }
            let take_end = e_end.min(off + want as u64);
            if pos < e.laddr {
                // gap (sparse) — zero fill
                let gap = (e.laddr - pos).min((want - filled) as u64) as usize;
                out[filled..filled + gap].fill(0);
                filled += gap;
                pos += gap as u64;
                if filled == want {
                    break;
                }
                continue;
            }
            let from = pos - e.laddr;
            let n = (take_end - pos) as usize;
            let b = self
                .dmg
                .read(e.pbn * self.bs as u64 + from, n)?;
            out[filled..filled + n].copy_from_slice(&b);
            filled += n;
            pos += n as u64;
        }
        Ok(filled)
    }

    /// Walk with everything mkiosroot needs: (path, fid, is_dir, first
    /// physical block) — sorted lexically (parents before children).
    pub fn walk_meta(&self) -> Vec<(String, u64, bool, u64)> {
        let mut out: Vec<(String, u64, bool, u64)> = Vec::new();
        for (path, fid) in self.walk_all() {
            // authoritative: the inode's S_IFDIR bits (empty dirs like /dev
            // have no children and would otherwise be treated as files)
            let is_dir = self.is_dir_fid(fid);
            let paddr = self
                .extents_for(fid)
                .and_then(|e| e.iter().map(|e| e.pbn).min())
                .unwrap_or(u64::MAX);
            out.push((path, fid, is_dir, paddr));
        }
        out.sort();
        out
    }

    /// decmpfs raw record for a file id (for provenance diagnostics).
    pub fn xattr(&self, fid: u64, name: &str) -> Option<&Vec<u8>> {
        self.xattrs.get(&fid)?.get(name)
    }

    /// Full file bytes, transparently decompressing decmpfs content.
    pub fn read_file(&mut self, fid: u64) -> Result<Vec<u8>> {
        let (mode, size) = (
            self.inodes.get(&fid).map(|(m, _, _, _)| *m).unwrap_or(0),
            self.file_size(fid),
        );
        let _ = mode;
        if size == 0 {
            bail!("no data for inode {fid}");
        }
        if let Some(exts) = self.extents_for(fid) {
            return self.read_extents(&exts, size);
        }
        // transparent compression via decmpfs (+ resource fork dstream)
        let dec = self
            .xattrs
            .get(&fid)
            .and_then(|m| m.get(XS_DECMPFS))
            .cloned()
            .ok_or_else(|| crate::Error::Msg(format!("no data for inode {fid}")))?;
        // decmpfs record (j_xattr_val header at 0, fpmc header at 4)
        let (xflags, xlen) = (u16le(&dec, 0), u16le(&dec, 2) as usize);
        let payload = &dec[4..(4 + xlen).min(dec.len())];
        if payload.len() < 16 || &payload[0..4] != b"fpmc" {
            bail!("bad cmpf header");
        }
        let algo = u32le(payload, 4);
        let usize_ = u64le(payload, 8);
        let _ = xflags;
        // rsrc content, already resolved (inline or via the dstream file)
        let rsrc_data = self.xattr_resolved(fid, XS_RSRC)?;
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
                if std::env::var("SWEET_DECOMP_DEBUG").is_ok() {
                    let b0 = data_offset as usize + 4;
                    eprintln!(
                        "algo4: rsrc {} bytes, data_offset={data_offset:#x} entries@{b0}={} first_entry={:02x?} chunk0={:02x?}",
                        rsrc.len(),
                        u32le(rsrc, b0),
                        &rsrc[b0 + 4..b0 + 12],
                        &rsrc[b0 + u32le(rsrc, b0 + 4) as usize..b0 + u32le(rsrc, b0 + 4) as usize + 8]
                    );
                }
                let base = data_offset as usize + 4;
                let entries = u32le(rsrc, base) as usize;
                let mut out = vec![0u8; (usize_ + 0xFFFF) & !0xFFFF];
                for k in 0..entries {
                    let (off, ln) = {
                        let o = base + 4 + k * 8;
                        (u32le(rsrc, o) as usize, u32le(rsrc, o + 4) as usize)
                    };
                    let src = &rsrc[base + off..(base + off + ln).min(rsrc.len())];
                    // the last chunk inflates to fewer than 64K
                    let left = usize_ - k * 0x10000;
                    let dcap = left.min(0x10000);
                    let dst = &mut out[k * 0x10000..k * 0x10000 + dcap];
                    if src[0] == 0x78 {
                        if let Err(e) = crate::ipsw::inflate_into(src, dst) {
                            if std::env::var("SWEET_DECOMP_DEBUG").is_ok() {
                                eprintln!("algo4: chunk {k} failed: {e} (src {ln}B, rsrcoff {:#x})", base + off);
                                continue;
                            }
                            bail!("zlib chunk {k} (src {ln}B): {e}");
                        }
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
    if o + 4 > b.len() {
        panic!("be32 out of range: len {} at {}", b.len(), o);
    }
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
