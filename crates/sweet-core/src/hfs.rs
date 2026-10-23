//! HFS+ / HFSX volume writer — builds a mountable XNU HFS+ root image
//! bottom-up (no host mount, no root): data blocks stream straight into the
//! image file, then catalog/extents B-trees and the volume header are laid
//! down at finish. This replaces mkfs.hfsplus + sudo mount + rsync from the
//! upstream phone flow so the whole pipeline stays root-free.
//!
//! Layout: [b0 boot][b1 volume header][allocation bitmap][extents tree]
//!         [file data...][catalog tree][alt volume header][reserved]
//!
//! Features: case-sensitive HFSX (v5, binary compare catalog), uid/gid/mode
//! from the source volume, symlinks ('slnk'/'rhap'), block-sharing (the dyld
//! shared-cache cryptex "hard links" reuse extents without duplicating
//! bytes), directories, plain files. No journal (mounts rw fine; the upstream
//! image journals for panic replay only).

use crate::error::Error;
use crate::{bail, Result};
use std::io::{Seek, SeekFrom, Write};

pub const BLOCK: u32 = 4096;
pub const ROOT_PARENT: u32 = 1;
pub const ROOT_ID: u32 = 2;

const VH_SIG: u16 = 0x482B; // 'H+'
const HFS_VERSION: u16 = 5; // HFSX (case-sensitive)
const LAST_MOUNTED: u32 = 0x3130_2E31; // '10.1'
const VOL_UNMOUNTED: u32 = 0x0000_0100; // clean
const ATTR_FILE: u32 = 1; // catalog record type: folder
const ATTR_DIR: u32 = 1; // (folder record)
const REC_FOLDER: u16 = 1;
const REC_FILE: u16 = 2;
const REC_FOLDER_THREAD: u16 = 3;
const REC_FILE_THREAD: u16 = 4;
const KEY_BINARY: u8 = 0xBC; // kHFSBinaryCompare (case-sensitive HFSX)
const NODE_LEAF: i8 = -1;
const NODE_INDEX: i8 = 0;
const NODE_HEADER: i8 = 1;
const HFS_EPOCH_OFFSET: u64 = 2_082_844_800; // unix -> HFS (1904)

#[derive(Clone)]
struct ForkExt {
    start: u32,
    count: u32,
}

#[derive(Clone)]
enum Kind {
    Dir,
    File,
    Symlink(String),
}

struct Node {
    id: u32,
    parent: u32,
    name: String, // ASCII (iOS rootfs is ASCII; bail on anything else)
    kind: Kind,
    uid: u32,
    gid: u32,
    mode: u16,
    size: u64,
    extents: Vec<ForkExt>,
    create: u32,
    modify: u32,
}

pub struct HfsImage {
    f: std::fs::File,
    pub total_blocks: u32,
    next_block: u32,
    next_cnid: u32,
    nodes: Vec<Node>,
    pub files_written: u64,
    pub bytes_written: u64,
    now: u32,
}

impl HfsImage {
    /// Create (truncate) the image file sized `total_blocks` * 4096.
    pub fn create(path: &std::path::Path, total_blocks: u32) -> Result<Self> {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        f.set_len(total_blocks as u64 * BLOCK as u64)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32
            + HFS_EPOCH_OFFSET as u32;
        Ok(Self {
            f,
            total_blocks,
            next_block: 0,
            next_cnid: 16, // 1..15 reserved by the format
            nodes: Vec::new(),
            files_written: 0,
            bytes_written: 0,
            now,
        })
    }

    fn alloc(&mut self, blocks: u32) -> Result<u32> {
        if self.next_block + blocks > self.total_blocks - 2 {
            bail!(
                "image full: need {blocks} blocks at {}, total {}",
                self.next_block,
                self.total_blocks
            );
        }
        let b = self.next_block;
        self.next_block += blocks;
        Ok(b)
    }

    fn new_node(
        &mut self,
        parent: u32,
        name: &str,
        kind: Kind,
        uid: u32,
        gid: u32,
        mode: u16,
        size: u64,
    ) -> Result<u32> {
        if self.next_cnid > 0xFFFF_FFF0 {
            bail!("cnid exhausted");
        }
        if !name.is_ascii() {
            bail!("non-ASCII name not supported: {name:?}");
        }
        let id = self.next_cnid;
        self.next_cnid += 1;
        self.nodes.push(Node {
            id,
            parent,
            name: name.to_string(),
            kind,
            uid,
            gid,
            mode,
            size,
            extents: Vec::new(),
            create: self.now,
            modify: self.now,
        });
        Ok(id)
    }

    /// Register a directory (content added via later add_* calls).
    pub fn add_dir(&mut self, parent: u32, name: &str, uid: u32, gid: u32, mode: u16) -> Result<u32> {
        self.new_node(parent, name, Kind::Dir, uid, gid, mode, 0)
    }

    /// Add a file by streaming `size` bytes from `src` (called once per chunk
    /// by the closure; every byte must be written). Data blocks are
    /// contiguous. Returns the cnid.
    pub fn add_file(
        &mut self,
        parent: u32,
        name: &str,
        uid: u32,
        gid: u32,
        mode: u16,
        size: u64,
        src: &mut dyn FnMut(&mut Self, u64, u64) -> Result<()>,
    ) -> Result<u32> {
        let blocks = (size.div_ceil(BLOCK as u64)) as u32;
        let start = self.alloc(blocks)?;
        let id = self.new_node(parent, name, Kind::File, uid, gid, mode, size)?;
        let node = &mut self.nodes[id as usize - 16];
        node.extents = vec![ForkExt { start, count: blocks }];
        drop(node);
        // stream the content into the freshly allocated blocks
        self.f.seek(SeekFrom::Start(start as u64 * BLOCK as u64))?;
        let mut left = size;
        while left > 0 {
            let chunk = left.min(8 << 20);
            src(self, size - left, chunk)?;
            left -= chunk;
        }
        self.files_written += 1;
        self.bytes_written += size;
        Ok(id)
    }

    /// Zero-fill a hole of `size` bytes for an empty/sparse file.
    pub fn add_file_empty(
        &mut self,
        parent: u32,
        name: &str,
        uid: u32,
        gid: u32,
        mode: u16,
        size: u64,
    ) -> Result<u32> {
        self.add_file(parent, name, uid, gid, mode, size, &mut |s, off, len| {
            s.f.seek(SeekFrom::Start(
                s.nodes[s.nodes.len() - 1].extents[0].start as u64 * BLOCK as u64 + off,
            ))?;
            let zeros = vec![0u8; len as usize];
            s.f.write_all(&zeros)?;
            Ok(())
        })
    }

    /// Register a file whose content REUSES another file's blocks (the DSC
    /// cache hard-link trick: identical bytes, one physical copy).
    pub fn add_file_shared(&mut self, parent: u32, name: &str, src_id: u32) -> Result<u32> {
        let src = self
            .nodes
            .iter()
            .find(|n| n.id == src_id)
            .ok_or_else(|| Error::Msg("add_file_shared: source not found".into()))?;
        let (uid, gid, mode, size, extents) =
            (src.uid, src.gid, src.mode, src.size, src.extents.clone());
        let id = self.new_node(parent, name, Kind::File, uid, gid, mode, size)?;
        self.nodes[id as usize - 16].extents = extents;
        Ok(id)
    }

    /// Add a symlink (content = target path).
    pub fn add_symlink(
        &mut self,
        parent: u32,
        name: &str,
        uid: u32,
        gid: u32,
        mode: u16,
        target: &str,
    ) -> Result<u32> {
        let bytes = target.as_bytes();
        let blocks = (bytes.len().div_ceil(BLOCK as usize)) as u32;
        let start = self.alloc(blocks)?;
        let id = self.new_node(
            parent,
            name,
            Kind::Symlink(target.to_string()),
            uid,
            gid,
            mode,
            bytes.len() as u64,
        )?;
        self.nodes[id as usize - 16].extents = vec![ForkExt { start, count: blocks }];
        self.f
            .seek(SeekFrom::Start(start as u64 * BLOCK as u64))?;
        self.f.write_all(bytes)?;
        let pad = (blocks as usize * BLOCK as usize) - bytes.len();
        if pad > 0 {
            self.f.write_all(&vec![0u8; pad])?;
        }
        Ok(id)
    }

    /// Lay down bitmap, B-trees, volume headers. The image is complete
    /// (and mountable) after this returns.
    pub fn finish(mut self, volume_name: &str) -> Result<std::path::PathBuf> {
        let total = self.total_blocks;
        // ---- reserve trailing metadata: catalog (built below) + alt VH ----
        // The catalog is built into memory-sized leaf/index nodes first so we
        // know its block count, then written after the data blocks.
        let (catalog, node_count, root_node, first_leaf, last_leaf, leaf_records, max_key) =
            build_catalog(&self.nodes, volume_name)?;
        let cat_blocks = (catalog.len() as u32).div_ceil(BLOCK);
        let cat_start = self.alloc(cat_blocks)?;

        // ---- allocation bitmap ----
        let bmap_bytes = (total as usize).div_ceil(8);
        let mut bmap = vec![0u8; bmap_bytes.div_ceil(BLOCK as usize) * BLOCK as usize];
        let setbit = |bm: &mut [u8], b: u32| bm[(b / 8) as usize] |= 0x80 >> (b % 8);
        // reserved blocks: 0 (boot), 1 (VH), bitmap itself, extents tree, data, catalog
        let bmap_blocks = bmap.len() as u32 / BLOCK;
        let ext_start = 2 + bmap_blocks;
        let ext_blocks = 1u32; // empty extents tree: one header node
        for b in 0..2 + bmap_blocks + ext_blocks {
            setbit(&mut bmap, b);
        }
        let mut cursor = ext_start + ext_blocks;
        // re-derive data allocations from the nodes (they were allocated
        // sequentially starting right after the extents tree)
        for n in &self.nodes {
            for e in &n.extents {
                for b in e.start..e.start + e.count {
                    setbit(&mut bmap, b);
                }
            }
        }
        for b in cat_start..cat_start + cat_blocks {
            setbit(&mut bmap, b);
        }
        setbit(&mut bmap, total - 2); // alt VH
        setbit(&mut bmap, total - 1); // reserved
        cursor = cursor.max(if self.nodes.is_empty() {
            cursor
        } else {
            let max_end = self
                .nodes
                .iter()
                .flat_map(|n| n.extents.iter())
                .map(|e| e.start + e.count)
                .max()
                .unwrap_or(cursor);
            max_end
        });
        let used = cursor.max(cat_start + cat_blocks).max(total);
        let free = total - used;

        // write bitmap
        self.f
            .seek(SeekFrom::Start(2u64 * BLOCK as u64))?;
        self.f.write_all(&bmap)?;

        // ---- extents overflow tree: single empty header node ----
        let ext_node = empty_btree_header_node(4096, 516);
        self.f.seek(SeekFrom::Start(ext_start as u64 * BLOCK as u64))?;
        self.f.write_all(&ext_node)?;

        // ---- catalog ----
        self.f
            .seek(SeekFrom::Start(cat_start as u64 * BLOCK as u64))?;
        self.f.write_all(&catalog)?;

        // ---- volume header ----
        let file_count = self
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, Kind::File | Kind::Symlink(_)))
            .count() as u32;
        let folder_count = self
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, Kind::Dir))
            .count() as u32
            + 1; // root folder itself
        let vh = volume_header(&VhArgs {
            total_blocks: total,
            free_blocks: free,
            next_allocation: used,
            file_count,
            folder_count,
            next_catalog_id: self.next_cnid,
            bmap_start: 2,
            bmap_blocks,
            ext_start,
            ext_blocks,
            cat_start,
            cat_blocks,
            root_node,
            first_leaf,
            last_leaf,
            leaf_records,
            node_count,
            max_key,
            name: volume_name,
            now: self.now,
        });
        self.f.seek(SeekFrom::Start(BLOCK as u64))?;
        self.f.write_all(&vh)?;
        self.f
            .seek(SeekFrom::Start((total as u64 - 2) * BLOCK as u64))?;
        self.f.write_all(&vh)?;

        let path = self.f_path();
        Ok(path)
    }

    fn f_path(&self) -> std::path::PathBuf {
        // best-effort path via /proc/self/fd is overkill; caller knows it
        std::path::PathBuf::new()
    }
}

struct VhArgs<'a> {
    total_blocks: u32,
    free_blocks: u32,
    next_allocation: u32,
    file_count: u32,
    folder_count: u32,
    next_catalog_id: u32,
    bmap_start: u32,
    bmap_blocks: u32,
    ext_start: u32,
    ext_blocks: u32,
    cat_start: u32,
    cat_blocks: u32,
    root_node: u32,
    first_leaf: u32,
    last_leaf: u32,
    leaf_records: u32,
    node_count: u32,
    max_key: u16,
    name: &'a str,
    now: u32,
}

fn fork_data(size: u64, extents: &[ForkExt]) -> [u8; 80] {
    let mut fd = [0u8; 80];
    fd[0..8].copy_from_slice(&size.to_be_bytes());
    fd[8..12].copy_from_slice(&BLOCK.to_be_bytes()); // clump
    fd[12..16].copy_from_slice(&(size.div_ceil(BLOCK as u64) as u32).to_be_bytes());
    for (i, e) in extents.iter().take(8).enumerate() {
        let o = 16 + i * 8;
        fd[o..o + 4].copy_from_slice(&e.start.to_be_bytes());
        fd[o + 4..o + 8].copy_from_slice(&e.count.to_be_bytes());
    }
    fd
}

fn volume_header(a: &VhArgs) -> [u8; 512] {
    let mut v = [0u8; 512];
    v[0..2].copy_from_slice(&VH_SIG.to_be_bytes());
    v[2..4].copy_from_slice(&HFS_VERSION.to_be_bytes());
    v[4..8].copy_from_slice(&VOL_UNMOUNTED.to_be_bytes());
    v[8..12].copy_from_slice(&LAST_MOUNTED.to_be_bytes());
    // journalInfoBlock = 0 (no journal)
    let (mut c, mut m) = (a.now, a.now);
    let _ = &mut c;
    let _ = &mut m;
    v[0x10..0x14].copy_from_slice(&a.now.to_be_bytes()); // createDate
    v[0x14..0x18].copy_from_slice(&a.now.to_be_bytes()); // modifyDate
    v[0x1C..0x20].copy_from_slice(&a.now.to_be_bytes()); // checkedDate
    v[0x20..0x24].copy_from_slice(&a.file_count.to_be_bytes());
    v[0x24..0x28].copy_from_slice(&a.folder_count.to_be_bytes());
    v[0x28..0x2C].copy_from_slice(&BLOCK.to_be_bytes());
    v[0x2C..0x30].copy_from_slice(&a.total_blocks.to_be_bytes());
    v[0x30..0x34].copy_from_slice(&a.free_blocks.to_be_bytes());
    v[0x34..0x38].copy_from_slice(&a.next_allocation.to_be_bytes());
    v[0x38..0x3C].copy_from_slice(&BLOCK.to_be_bytes()); // rsrc clump
    v[0x3C..0x40].copy_from_slice(&BLOCK.to_be_bytes()); // data clump
    v[0x40..0x44].copy_from_slice(&a.next_catalog_id.to_be_bytes());
    v[0x44..0x48].copy_from_slice(&1u32.to_be_bytes()); // writeCount
    v[0x48] = 1; // encodingsBitmap: bit 0 = Mac Roman
    v[0x50..0x54].copy_from_slice(b"iOS"); // finderInfo[0..2] = volume name hints
    // allocation file
    v[0x70..0x78].copy_from_slice(&(a.bmap_blocks as u64 * BLOCK as u64).to_be_bytes());
    v[0x78..0x82].copy_from_slice(&a.bmap_blocks.to_be_bytes());
    v[0x80..0x84].copy_from_slice(&a.bmap_start.to_be_bytes());
    v[0x84..0x88].copy_from_slice(&a.bmap_blocks.to_be_bytes());
    // extents file
    v[0xC0..0xC8].copy_from_slice(&(a.ext_blocks as u64 * BLOCK as u64).to_be_bytes());
    v[0xC8..0xCC].copy_from_slice(&a.ext_blocks.to_be_bytes());
    v[0xD0..0xD4].copy_from_slice(&a.ext_start.to_be_bytes());
    v[0xD4..0xD8].copy_from_slice(&a.ext_blocks.to_be_bytes());
    // catalog file
    v[0x110..0x118].copy_from_slice(&(a.cat_blocks as u64 * BLOCK as u64).to_be_bytes());
    v[0x118..0x11C].copy_from_slice(&a.cat_blocks.to_be_bytes());
    v[0x120..0x124].copy_from_slice(&a.cat_start.to_be_bytes());
    v[0x124..0x128].copy_from_slice(&a.cat_blocks.to_be_bytes());
    // attributes + startup: zero forks (absent)
    let _ = a.name;
    v
}

// ---------------- B-tree construction ----------------

fn unistr(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + name.len() * 2);
    out.extend_from_slice(&(name.chars().count() as u16).to_be_bytes());
    for c in name.encode_utf16() {
        out.extend_from_slice(&c.to_be_bytes());
    }
    out
}

fn cat_key_bytes(parent: u32, name: &str) -> Vec<u8> {
    // [keyLen u16][parent u32][uniStr]; keyLen = 6 + 2*chars (excludes itself)
    let uni = unistr(name);
    let keylen = (6 + uni.len() - 2) as u16; // 4 + 2 + 2*chars
    let mut out = Vec::with_capacity(2 + 4 + uni.len());
    out.extend_from_slice(&keylen.to_be_bytes());
    out.extend_from_slice(&parent.to_be_bytes());
    out.extend_from_slice(&uni);
    out
}

fn catalog_record_type(n: &Node) -> u16 {
    match n.kind {
        Kind::Dir => REC_FOLDER,
        Kind::File => REC_FILE,
        Kind::Symlink(_) => REC_FILE,
    }
}

fn folder_record(n: &Node, valence: u32) -> Vec<u8> {
    // HFSPlusCatalogFolder (88 bytes)
    let mut r = vec![0u8; 88];
    r[0..2].copy_from_slice(&REC_FOLDER.to_be_bytes());
    r[4..8].copy_from_slice(&valence.to_be_bytes());
    r[8..12].copy_from_slice(&n.id.to_be_bytes());
    r[12..16].copy_from_slice(&n.create.to_be_bytes());
    r[16..20].copy_from_slice(&n.modify.to_be_bytes());
    r[20..24].copy_from_slice(&n.modify.to_be_bytes()); // attr mod
    r[24..28].copy_from_slice(&n.modify.to_be_bytes()); // access
    r[32..36].copy_from_slice(&n.uid.to_be_bytes()); // bsd owner
    r[36..40].copy_from_slice(&n.gid.to_be_bytes()); // group
    r[44..46].copy_from_slice(&n.mode.to_be_bytes()); // mode (admin/owner flags 0)
    r
}

fn file_record(n: &Node, data_extents: &[ForkExt]) -> Vec<u8> {
    // HFSPlusCatalogFile (248 bytes)
    let mut r = vec![0u8; 248];
    r[0..2].copy_from_slice(&REC_FILE.to_be_bytes());
    r[8..12].copy_from_slice(&n.id.to_be_bytes());
    r[12..16].copy_from_slice(&n.create.to_be_bytes());
    r[16..20].copy_from_slice(&n.modify.to_be_bytes());
    r[20..24].copy_from_slice(&n.modify.to_be_bytes());
    r[24..28].copy_from_slice(&n.modify.to_be_bytes());
    r[32..36].copy_from_slice(&n.uid.to_be_bytes());
    r[36..40].copy_from_slice(&n.gid.to_be_bytes());
    r[44..46].copy_from_slice(&n.mode.to_be_bytes());
    match &n.kind {
        Kind::Symlink(_) => {
            // finderInfo: type 'slnk' creator 'rhap' at userInfo+? — the file
            // record has userInfo (8B @56) + finderInfo (8B @64); symlinks
            // store the type/creator in the *first* 8 bytes (userInfo).
            r[56..60].copy_from_slice(b"slnk");
            r[60..64].copy_from_slice(b"rhap");
        }
        _ => {}
    }
    let data = fork_data(n.size, data_extents);
    r[88..168].copy_from_slice(&data); // dataFork
    // resource fork: zero (80 bytes at 168..248)
    r
}

fn thread_record(kind: u16, parent: u32, name: &str) -> Vec<u8> {
    let uni = unistr(name);
    let mut r = vec![0u8; 2 + 2 + 4 + uni.len()];
    r[0..2].copy_from_slice(&kind.to_be_bytes());
    r[4..8].copy_from_slice(&parent.to_be_bytes());
    r[8..].copy_from_slice(&uni);
    r
}

/// Bottom-up catalog build. Returns (bytes, total_nodes, root_node,
/// first_leaf, last_leaf, leaf_records, max_key_len).
fn build_catalog(
    nodes: &[Node],
    volume_name: &str,
) -> Result<(Vec<u8>, u32, u32, u32, u32, u32, u16)> {
    const NODE: usize = 4096;
    const DESC: usize = 14;

    // ---- records: (sort_key, key_bytes, data_bytes) ----
    let mut recs: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = Vec::new();

    let mut valence: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for n in nodes {
        *valence.entry(n.parent).or_default() += 1;
    }
    let root_children = nodes.iter().filter(|n| n.parent == ROOT_ID).count() as u32;

    let root = Node {
        id: ROOT_ID,
        parent: ROOT_PARENT,
        name: String::new(),
        kind: Kind::Dir,
        uid: 0,
        gid: 0,
        mode: 0o755,
        size: 0,
        extents: vec![],
        create: 0,
        modify: 0,
    };
    recs.push((
        sort_key(ROOT_PARENT, ""),
        cat_key_bytes(ROOT_PARENT, ""),
        folder_record(&root, root_children),
    ));
    recs.push((
        sort_key(ROOT_ID, ""),
        cat_key_bytes(ROOT_ID, ""),
        thread_record(REC_FOLDER_THREAD, ROOT_PARENT, volume_name),
    ));

    for n in nodes {
        let data = match &n.kind {
            Kind::Dir => folder_record(n, *valence.get(&n.id).unwrap_or(&0)),
            Kind::File | Kind::Symlink(_) => file_record(n, &n.extents),
        };
        recs.push((sort_key(n.parent, &n.name), cat_key_bytes(n.parent, &n.name), data));
        let ttype = match n.kind {
            Kind::Dir => REC_FOLDER_THREAD,
            _ => REC_FILE_THREAD,
        };
        recs.push((sort_key(n.id, ""), cat_key_bytes(n.id, ""), thread_record(ttype, n.parent, &n.name)));
    }
    recs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut max_key = 0u16;
    for (key, ..) in &recs {
        max_key = max_key.max(key.len() as u16);
    }

    // ---- pack leaf nodes (height 1, chained via fLink/bLink) ----
    let mut leaves: Vec<Vec<u8>> = Vec::new();
    let mut leaf_records = 0u32;
    {
        let mut buf: Vec<u8> = vec![0; NODE];
        let mut offs: Vec<u16> = Vec::new();
        let mut data_end = DESC as u16;
        for (_, key, dat) in &recs {
            let total = (2 + key.len() + dat.len() + 1) & !1;
            if DESC + data_end as usize + total + 2 * (offs.len() + 2) > NODE {
                leaves.push(finish_node(&mut buf, &offs, NODE_LEAF, 1, data_end));
                leaf_records += offs.len() as u32;
                data_end = DESC as u16;
                offs.clear();
            }
            let off = data_end as usize;
            buf[off..off + 2].copy_from_slice(&(key.len() as u16).to_be_bytes());
            buf[off + 2..off + 2 + key.len()].copy_from_slice(key);
            buf[off + 2 + key.len()..off + 2 + key.len() + dat.len()].copy_from_slice(dat);
            data_end = (off + 2 + key.len() + dat.len() + 1) as u16 & !1;
            offs.push(off as u16);
        }
        if !offs.is_empty() {
            leaves.push(finish_node(&mut buf, &offs, NODE_LEAF, 1, data_end));
            leaf_records += offs.len() as u32;
        }
    }

    // ---- index levels: promote each child's first key (stored form) ----
    // level holds (first_key_stored, node_number)
    let mut level: Vec<(Vec<u8>, u32)> = leaves
        .iter()
        .enumerate()
        .map(|(i, n)| (first_key_of(n), 1 + i as u32))
        .collect();
    let first_leaf = 1u32;
    let last_leaf = leaves.len() as u32;
    let mut all_nodes: Vec<Vec<u8>> = Vec::new();
    all_nodes.extend(leaves.iter().cloned());
    let mut level_bounds: Vec<(u32, u32)> = vec![(first_leaf, last_leaf)]; // (first,last) node numbers per level
    let mut index_height = 1u8;

    let mut next_no = 1 + all_nodes.len() as u32;
    while level.len() > 1 {
        index_height += 1;
        let mut idx: Vec<Vec<u8>> = Vec::new();
        {
            let mut buf: Vec<u8> = vec![0; NODE];
            let mut offs: Vec<u16> = Vec::new();
            let mut data_end = DESC as u16;
            let mut children: Vec<u32> = Vec::new();
            for (key, child) in &level {
                let total = (2 + key.len() + 4 + 1) & !1;
                if DESC + data_end as usize + total + 2 * (offs.len() + 2) > NODE {
                    idx.push(finish_node_index(&mut buf, &offs, data_end, index_height));
                    data_end = DESC as u16;
                    offs.clear();
                    children.clear();
                }
                let off = data_end as usize;
                buf[off..off + 2].copy_from_slice(&(key.len() as u16).to_be_bytes());
                buf[off + 2..off + 2 + key.len()].copy_from_slice(key);
                buf[off + 2 + key.len()..off + 2 + key.len() + 4].copy_from_slice(&child.to_be_bytes());
                data_end = (off + 2 + key.len() + 4) as u16 & !1;
                offs.push(off as u16);
                children.push(*child);
            }
            if !offs.is_empty() {
                idx.push(finish_node_index(&mut buf, &offs, data_end, index_height));
            }
        }
        let base = next_no;
        for n in &idx {
            all_nodes.push(n.clone());
        }
        next_no += idx.len() as u32;
        level_bounds.push((base, next_no - 1));
        level = idx
            .iter()
            .enumerate()
            .map(|(i, n)| (first_key_of(n), base + i as u32))
            .collect();
    }
    let depth = index_height as u16 + 1; // leaves are height 1
    let root_node = next_no - 1;
    let total_nodes = 1 + all_nodes.len() as u32;

    // ---- fLink/bLink chains per level ----
    for (first, last) in &level_bounds {
        let mut no = *first;
        while no <= *last {
            let i = (no - 1) as usize;
            if i >= all_nodes.len() {
                break;
            }
            let f = if no < *last { no + 1 } else { 0 };
            let b = if no > *first { no - 1 } else { 0 };
            all_nodes[i][0..4].copy_from_slice(&(f as i32).to_be_bytes());
            all_nodes[i][4..8].copy_from_slice(&(b as i32).to_be_bytes());
            no += 1;
        }
    }

    // ---- assemble: node 0 = header, leaves, index levels ----
    let mut out: Vec<u8> = Vec::with_capacity(total_nodes as usize * NODE);
    let mut h = empty_btree_header_node(NODE, 516);
    patch_header(
        &mut h,
        depth,
        root_node,
        leaf_records,
        first_leaf,
        last_leaf,
        total_nodes,
    );
    out.extend_from_slice(&h);
    for n in &all_nodes {
        out.extend_from_slice(n);
    }
    Ok((out, total_nodes, root_node, first_leaf, last_leaf, leaf_records, max_key))
}

/// Patch the header-node BTHeaderRec (record at offset 14):
/// depth, root, leafRecords, first/last leaf, nodeSize, totalNodes.
fn patch_header(
    h: &mut [u8],
    depth: u16,
    root: u32,
    leaf_records: u32,
    first_leaf: u32,
    last_leaf: u32,
    total_nodes: u32,
) {
    let o = 14;
    h[o..o + 2].copy_from_slice(&depth.to_be_bytes());
    h[o + 2..o + 6].copy_from_slice(&root.to_be_bytes());
    h[o + 6..o + 10].copy_from_slice(&leaf_records.to_be_bytes());
    h[o + 10..o + 12].copy_from_slice(&(first_leaf as i16).to_be_bytes());
    h[o + 12..o + 14].copy_from_slice(&(last_leaf as i16).to_be_bytes());
    h[o + 14..o + 16].copy_from_slice(&4096i16.to_be_bytes());
    h[o + 18..o + 22].copy_from_slice(&total_nodes.to_be_bytes());
}

fn finish_node(buf: &[u8], offs: &[u16], kind: i8, height: u8, data_end: u16) -> Vec<u8> {
    let mut node = buf.to_vec();
    node[8] = kind as u8;
    node[10] = height;
    node[12..14].copy_from_slice(&(offs.len() as u16).to_be_bytes());
    let mut t = 4096usize;
    for o in offs {
        t -= 2;
        node[t..t + 2].copy_from_slice(&o.to_be_bytes());
    }
    t -= 2;
    node[t..t + 2].copy_from_slice(&data_end.to_be_bytes());
    node
}

fn finish_node_index(buf: &[u8], offs: &[u16], data_end: u16, height: u8) -> Vec<u8> {
    finish_node(buf, offs, NODE_INDEX, height, data_end)
}

/// First record's stored key ([keyLen u16][key bytes]) of a packed node.
fn first_key_of(node: &[u8]) -> Vec<u8> {
    let first_off = u16_from(&node[4094..4096]) as usize;
    let klen = u16_from(&node[first_off..first_off + 2]) as usize;
    node[first_off..first_off + 2 + klen].to_vec()
}

fn u16_from(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn sort_key(parent: u32, name: &str) -> Vec<u8> {
    let mut s = parent.to_be_bytes().to_vec();
    s.extend_from_slice(&name.encode_utf16().flat_map(|c| c.to_be_bytes()).collect::<Vec<u8>>());
    s
}

/// Empty extents-overflow B-tree: a single header node.
fn empty_btree_header_node(node_size: usize, max_key: u16) -> Vec<u8> {
    let mut n = vec![0u8; node_size];
    n[8] = NODE_HEADER as u8;
    n[12..14].copy_from_slice(&3u16.to_be_bytes());
    let h = 14;
    n[h..h + 2].copy_from_slice(&1i16.to_be_bytes());
    n[h + 2..h + 6].copy_from_slice(&0u32.to_be_bytes());
    n[h + 6..h + 10].copy_from_slice(&0u32.to_be_bytes());
    n[h + 10..h + 12].copy_from_slice(&0i16.to_be_bytes());
    n[h + 12..h + 14].copy_from_slice(&0i16.to_be_bytes());
    n[h + 14..h + 16].copy_from_slice(&(node_size as i16).to_be_bytes());
    n[h + 16..h + 18].copy_from_slice(&max_key.to_be_bytes());
    n[h + 18..h + 22].copy_from_slice(&1u32.to_be_bytes());
    n[h + 22..h + 26].copy_from_slice(&0u32.to_be_bytes());
    n[h + 26..h + 28].copy_from_slice(&0u16.to_be_bytes());
    n[h + 28..h + 32].copy_from_slice(&BLOCK.to_be_bytes());
    n[h + 32] = 0; // btreeType: kHFSBTreeType
    n[h + 33] = KEY_BINARY;
    n[h + 34..h + 38].copy_from_slice(&0u32.to_be_bytes());
    // userData (128) + map (128) stay zero; offsets:
    let mut o = node_size;
    for rec in [14usize, 14 + 106, 14 + 106 + 128] {
        o -= 2;
        n[o..o + 2].copy_from_slice(&(rec as u16).to_be_bytes());
    }
    o -= 2;
    n[o..o + 2].copy_from_slice(&((14 + 106 + 128 + 128) as u16).to_be_bytes());
    n
}
