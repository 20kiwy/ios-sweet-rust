//! mkiosroot — assemble the real iOS 16.7 HFS+ root image from the IPSW
//! volumes (root-free, pure Rust; parity with upstream tools/mkiosroot.sh +
//! tools/sweet-root-tweaks.py, which needed sudo mount + rsync).
//!
//! Profiles:
//! * `full` — everything (OS root + OS/App cryptexes + DSC cache links):
//!   ~11-12 GiB, the QEMU-parity root (built and booted on CI).
//! * `mini` — the boot-critical closure for the first userspace gate
//!   (launchd alive): DSC + launchd + dyld + launchd config + root skeleton
//!   + SWEET additions. A labeled subset, never claimed as full parity.

use crate::apfs::Apfs;
use crate::error::Result;
use crate::{bail};
use crate::hfs::{HfsImage, ROOT_ID};
use crate::plist::{parse as plist_parse, write as plist_write, Plist};
use std::collections::BTreeMap;
use std::path::Path;

const BUILD: &str = "20H392";
const VERSION: &str = "16.7.16";
const MOBILE: u32 = 501;
const ACTIVATION_CONTAINER: &str = "D90C94C4-F94E-4FCF-8A28-A93BE3846D8E";

pub struct Report {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub image_blocks: u32,
}

/// `mini` profile: SystemOS files included (the dyld shared cache).
fn mini_keep_sysos(path: &str) -> bool {
    path.starts_with("/System/Library/Caches/com.apple.dyld/")
}

/// `mini` profile inclusion (boot-critical closure for launchd).
fn mini_keep(path: &str, is_dir: bool) -> bool {
    let top_level = !path[1..].contains('/');
    let top_symlink = !is_dir && top_level;
    // top-level real directories ride along empty (dir records are free):
    // /dev (devfs mountpoint — bsd_init.c devfs_kernel_mount), /cores, /bin…
    let top_dir = is_dir && top_level;
    let prefixes = [
        "/private/etc/",
        "/System/Library/LaunchDaemons/",
        "/System/Library/xpc/",
        "/System/Library/CoreServices/",
        "/usr/lib/",
        "/usr/libexec/",
        "/usr/bin/",
        "/usr/sbin/",
        "/sbin/",
        "/usr/local/",
        "/usr/share/",
    ];
    prefixes.iter().any(|p| path.starts_with(p)) || top_symlink || top_dir
}

struct Builder<'a> {
    img: HfsImage,
    index: BTreeMap<String, u32>,
    seen: BTreeMap<(u8, u64), u32>,
    bytes: u64,
    files: u64,
    dirs: u64,
    skipped: u64,
    tweaks: bool,
    progress: &'a dyn Fn(&str),
}

impl<'a> Builder<'a> {
    fn ensure_dir_path(&mut self, path: &str, uid: u32, gid: u32, mode: u16) -> Result<u32> {
        if path == "/" {
            return Ok(ROOT_ID);
        }
        let mut parent = ROOT_ID;
        let mut acc = String::new();
        for seg in path.trim_start_matches('/').split('/') {
            if seg.is_empty() {
                continue;
            }
            acc = format!("{acc}/{seg}");
            parent = match self.index.get(&acc) {
                Some(id) => *id,
                None => {
                    let id = self.img.add_dir(parent, seg, uid, gid, mode)?;
                    self.index.insert(acc.clone(), id);
                    self.dirs += 1;
                    id
                }
            };
        }
        Ok(parent)
    }

    fn put_file(
        &mut self,
        vol: &mut Apfs,
        tag: u8,
        fid: u64,
        path: &str,
        parent: u32,
        name: &str,
    ) -> Result<()> {
        if let Some(cnid) = self.seen.get(&(tag, fid)) {
            let cnid = *cnid;
            let id = self.img.add_file_shared(parent, name, cnid)?;
            self.index.insert(path.to_string(), id);
            return Ok(());
        }
        let Some((mode, uid, gid, size)) = vol.stat(fid) else {
            self.skipped += 1;
            return Ok(());
        };
        let kind = mode & 0xF000;
        if kind == 0xA000 {
            // APFS stores the symlink target in the com.apple.fs.symlink
            // xattr (value = u16 flags + target text)
            let t = match vol.xattr(fid, "com.apple.fs.symlink") {
                Some(v) if v.len() >= 2 => {
                    String::from_utf8_lossy(&v[2..]).trim_end_matches('\0').to_string()
                }
                _ => {
                    let target = vol.read_file(fid)?;
                    String::from_utf8_lossy(&target).trim_end_matches('\0').to_string()
                }
            };
            let id = self.img.add_symlink(parent, name, uid, gid, mode, &t)?;
            self.index.insert(path.to_string(), id);
            self.seen.insert((tag, fid), id);
            return Ok(());
        }
        if kind != 0x8000 {
            self.skipped += 1;
            return Ok(());
        }
        // excluded: VirtualAudio HAL plugin (mediaserverd crash loop, v12)
        if path.starts_with("/Library/Audio/Plug-Ins/HAL/VirtualAudio.plugin") {
            self.skipped += 1;
            return Ok(());
        }
        // decmpfs algo 14 (LZBITMAP) is not implemented yet: the affected
        // files are DSC companion metadata (.dyldlinkedit etc), not boot-
        // critical. Count and skip honestly.
        if let Some(dec) = vol.xattr(fid, "com.apple.decmpfs") {
            if dec.len() >= 12 && &dec[4..8] == b"fpmc" {
                let algo = u32::from_le_bytes(dec[8..12].try_into().unwrap());
                if algo == 14 {
                    self.skipped += 1;
                    return Ok(());
                }
            }
        }
        // plist surgery: edit before writing
        let modified: Option<Vec<u8>> = if self.tweaks {
            if path == "/System/Library/xpc/launchd.plist" {
                Some(tweak_launchd(vol, fid)?)
            } else if path.contains("MediaToolbox.framework")
                && path.ends_with("SubServerConfiguration.plist")
            {
                Some(tweak_subserver(vol, fid)?)
            } else {
                None
            }
        } else {
            None
        };
        let id = if let Some(bytes) = modified {
            let size = bytes.len() as u64;
            self.img.add_file(parent, name, uid, gid, mode, size, &mut |img, off, len| {
                img.write_file_chunk(off, &bytes[off as usize..(off + len) as usize])
            })?
        } else if vol.xattr(fid, "com.apple.decmpfs").is_some() {
            // compressed: decompress whole, then serve from the buffer
            let whole = match vol.read_file(fid) {
                Ok(b) => b,
                Err(e) => bail!("decmpfs read failed for {path}: {e}"),
            };
            if whole.len() as u64 != size {
                bail!("decmpfs size mismatch for {path}: {} vs {size}", whole.len());
            }
            self.img.add_file(parent, name, uid, gid, mode, size, &mut |img, off, len| {
                img.write_file_chunk(off, &whole[off as usize..(off + len) as usize])
            })?
        } else {
            // plain: stream from the volume in chunks
            self.img.add_file(parent, name, uid, gid, mode, size, &mut |img, off, len| {
                let mut buf = vec![0u8; len as usize];
                let got = vol.read_range(fid, off, &mut buf)?;
                if got < buf.len() {
                    buf[got..].fill(0);
                }
                img.write_file_chunk(off, &buf)
            })?
        };
        self.seen.insert((tag, fid), id);
        self.index.insert(path.to_string(), id);
        self.bytes += size;
        self.files += 1;
        Ok(())
    }

    fn put_created(&mut self, path: &str, uid: u32, gid: u32, mode: u16, data: &[u8]) -> Result<()> {
        let parent_path = match path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(i) => path[..i].to_string(),
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        let parent = self.ensure_dir_path(&parent_path, uid, gid, 0o755)?;
        let id = self.img.add_file(parent, name, uid, gid, mode, data.len() as u64, &mut |img, off, len| {
            img.write_file_chunk(off, &data[off as usize..(off + len) as usize])
        })?;
        self.index.insert(path.to_string(), id);
        self.files += 1;
        self.bytes += data.len() as u64;
        Ok(())
    }
}

fn tweak_launchd(vol: &mut Apfs, fid: u64) -> Result<Vec<u8>> {
    let raw = vol.read_file(fid)?;
    let mut p = plist_parse(&raw)?;
    if let Some(d) = p.as_dict_mut() {
        if let Some(Plist::Dict(daemons)) = d.get_mut("LaunchDaemons") {
            let mut renamed = 0usize;
            for (_, job) in daemons.iter_mut() {
                if let Some(jd) = job.as_dict_mut() {
                    if let Some(v) = jd.remove("_PanicOnCrash") {
                        jd.insert("_PanicOnCrasX".into(), v);
                        renamed += 1;
                    }
                }
            }
            let before = daemons.len();
            daemons.retain(|_, job| {
                job.as_dict()
                    .and_then(|d| d.get("Label"))
                    .and_then(|l| l.as_str())
                    != Some("com.apple.CommCenter")
            });
            progress_note(&format!(
                "launchd.plist: {renamed} _PanicOnCrash renamed, {} CommCenter jobs removed ({} -> {})",
                before - daemons.len(),
                before,
                daemons.len()
            ));
        }
    }
    plist_write(&p)
}

fn progress_note(msg: &str) {
    eprintln!("[mkiosroot] {msg}");
}

fn tweak_subserver(vol: &mut Apfs, fid: u64) -> Result<Vec<u8>> {
    let raw = vol.read_file(fid)?;
    let mut p = plist_parse(&raw)?;
    if let Some(d) = p.as_dict_mut() {
        d.insert("StartRecorderServer".into(), Plist::Bool(false));
    }
    plist_write(&p)
}

fn xml_plist(body: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n{body}\n</plist>\n"
    )
    .into_bytes()
}

/// Build the root image.
#[allow(clippy::too_many_arguments)]
pub fn build(
    os: &mut Apfs,
    mut sysos: Option<(&mut Apfs, Option<std::path::PathBuf>)>,
    mut appos: Option<&mut Apfs>,
    out: &Path,
    profile: &str,
    tweaks: bool,
    progress: &dyn Fn(&str),
) -> Result<Report> {
    // ---- pass 1: plan ----
    progress("walk: OS volume");
    let os_meta = os.walk_meta();
    let mut plan_files: Vec<(String, u64, u64)> = Vec::new(); // (path, fid, paddr)
    let mut plan_dirs: Vec<String> = Vec::new();
    let mut total_bytes = 0u64;
    for (path, fid, is_dir, paddr) in &os_meta {
        if path.contains('\u{2400}') {
            continue; // HFS+ private dirs
        }
        if profile == "mini" && !mini_keep(path, *is_dir) {
            continue;
        }
        if *is_dir {
            plan_dirs.push(path.clone());
        } else {
            if let Some((_, _, _, size)) = os.stat(*fid) {
                total_bytes += size.div_ceil(4096) * 4096;
            }
            plan_files.push((path.clone(), *fid, *paddr));
        }
    }

    let mut sys_files: Vec<(String, u64, u64)> = Vec::new();
    let mut sys_dirs: Vec<String> = Vec::new();
    let mut app_files: Vec<(String, u64, u64)> = Vec::new();
    let mut app_dirs: Vec<String> = Vec::new();
    let mut dsc_direct = false;
    if let Some((v, _unlink)) = sysos.as_mut() {
        progress("walk: SystemOS cryptex");
        for (path, fid, is_dir, paddr) in v.walk_meta() {
            if path.contains('\u{2400}') {
                continue;
            }
            if profile == "mini" {
                if is_dir || !mini_keep_sysos(&path) {
                    continue;
                }
                // place the DSC directly where dyld looks at ignition_level=1
                let name = path.rsplit('/').next().unwrap_or(&path);
                let dst = format!("/System/Library/Caches/com.apple.dyld/{name}");
                if let Some((_, _, _, size)) = v.stat(fid) {
                    total_bytes += size.div_ceil(4096) * 4096;
                }
                dsc_direct = true;
                sys_files.push((dst, fid, paddr));
                continue;
            }
            let dst = format!("/private/preboot/Cryptexes/OS{path}");
            if is_dir {
                sys_dirs.push(dst);
            } else {
                if let Some((_, _, _, size)) = v.stat(fid) {
                    total_bytes += size.div_ceil(4096) * 4096;
                }
                sys_files.push((dst, fid, paddr));
            }
        }
    }
    let _ = dsc_direct;
    if let Some(v) = appos.as_deref_mut() {
        progress("walk: AppOS cryptex");
        for (path, fid, is_dir, paddr) in v.walk_meta() {
            if path.contains('\u{2400}') {
                continue;
            }
            let dst = format!("/private/preboot/Cryptexes/App{path}");
            if is_dir {
                app_dirs.push(dst);
            } else {
                if let Some((_, _, _, size)) = v.stat(fid) {
                    total_bytes += size.div_ceil(4096) * 4096;
                }
                app_files.push((dst, fid, paddr));
            }
        }
    }

    // ---- image sizing (bitmap size depends on total_blocks: converge) ----
    let est_catalog: u64 = 320 * (1 << 20); // worst case for ~700k records
    let headroom: u64 = if profile == "full" {
        512 * (1 << 20)
    } else {
        64 * (1 << 20)
    };
    let mut total_blocks = ((total_bytes + est_catalog + headroom) / 4096) as u32;
    loop {
        let bmap_blocks = (total_blocks as u64).div_ceil(8).div_ceil(4096) as u64;
        let need = (total_bytes + (bmap_blocks + 1) * 4096 + est_catalog + headroom) / 4096;
        let nb = need as u32 + 2;
        if nb <= total_blocks {
            break;
        }
        total_blocks = nb + 64;
    }
    progress(&format!(
        "plan: {} files / {} dirs, data {:.2} GiB, image {} blocks ({:.2} GiB)",
        plan_files.len() + sys_files.len() + app_files.len(),
        plan_dirs.len() + sys_dirs.len() + app_dirs.len(),
        total_bytes as f64 / (1 << 30) as f64,
        total_blocks,
        total_blocks as f64 * 4096.0 / (1 << 30) as f64
    ));

    // ---- create + populate ----
    let img = HfsImage::create(out, total_blocks)?;
    let mut b = Builder {
        img,
        index: BTreeMap::new(),
        seen: BTreeMap::new(),
        bytes: 0,
        files: 0,
        dirs: 0,
        skipped: 0,
        tweaks,
        progress,
    };
    b.index.insert("/".into(), ROOT_ID);

    for d in plan_dirs.iter().chain(sys_dirs.iter()).chain(app_dirs.iter()) {
        b.ensure_dir_path(d, 0, 0, 0o755)?;
    }

    // ---- VFS mountpoints + boot-critical dirs the volume may not carry ----
    // XNU mounts devfs ON /dev (bsd_init.c devfs_kernel_mount) and launchd
    // opens /dev/null for fd 0; a missing mountpoint was the M3 wall
    // ("failed to reserve stdin descriptor: /dev/null: 2"). Upstream's
    // apfs-fuse + rsync flow synthesized some of these; our writer creates
    // them explicitly. Idempotent with the walk results.
    for (d, mode) in [
        ("/dev", 0o755u16),
        ("/mnt", 0o755),
        ("/private/tmp", 0o1777),
        ("/private/var/tmp", 0o1777),
        ("/private/var/run", 0o755),
        ("/private/var/vm", 0o755),
        ("/private/var/empty", 0o711),
        ("/private/var/root", 0o750),
        ("/private/var/db/launchd.db", 0o755),
        ("/private/var/db/xpc", 0o755),
    ] {
        b.ensure_dir_path(d, 0, 0, mode)?;
    }

    plan_files.sort_by_key(|(_, _, p)| *p);
    sys_files.sort_by_key(|(_, _, p)| *p);
    app_files.sort_by_key(|(_, _, p)| *p);

    let n_files = (plan_files.len() + sys_files.len() + app_files.len()) as u64;
    let mut done = 0u64;
    for (path, fid, _) in &plan_files {
        let (parent, name) = split_parent(path);
        let parent = b.ensure_dir_path(&parent, 0, 0, 0o755)?;
        b.put_file(os, 0, *fid, path, parent, name)?;
        done += 1;
        if done % 20000 == 0 {
            progress(&format!("os: {done}/{n_files} files"));
        }
    }
    if let Some((v, unlink)) = sysos.as_mut() {
        for (path, fid, _) in &sys_files {
            let (parent, name) = split_parent(path);
            let parent = b.ensure_dir_path(&parent, 0, 0, 0o755)?;
            b.put_file(v, 1, *fid, path, parent, name)?;
            done += 1;
            if done % 5000 == 0 {
                progress(&format!("sysos: {done}/{n_files} files"));
            }
        }
        if let Some(p) = unlink {
            // free the cryptex dmg before the catalog build (local disk fit);
            // the closed fd no longer needs the file
            progress("sysos: unlinking source dmg");
            let _ = std::fs::remove_file(p);
        }
    }
    if let Some(v) = appos.as_deref_mut() {
        for (path, fid, _) in &app_files {
            let (parent, name) = split_parent(path);
            let parent = b.ensure_dir_path(&parent, 0, 0, 0o755)?;
            b.put_file(v, 2, *fid, path, parent, name)?;
        }
    }

    // ---- DSC cache links (shared extents: zero extra bytes) ----
    if sysos.is_some() && profile == "full" {
        progress("link: dyld shared cache -> /System/Library/Caches/com.apple.dyld");
        let dsc_dir = "/private/preboot/Cryptexes/OS/System/Library/Caches/com.apple.dyld";
        let dst_dir = "/System/Library/Caches/com.apple.dyld";
        let names: Vec<(String, u32)> = b
            .index
            .iter()
            .filter(|(p, _)| p.starts_with(&format!("{dsc_dir}/")))
            .map(|(p, id)| (p[dsc_dir.len() + 1..].to_string(), *id))
            .collect();
        let parent = b.ensure_dir_path(dst_dir, 0, 0, 0o755)?;
        for (name, cnid) in names {
            let id = b.img.add_file_shared(parent, &name, cnid)?;
            b.index.insert(format!("{dst_dir}/{name}"), id);
        }
    }

    // ---- SWEET additions (mkiosroot.sh + sweet-root-tweaks parity) ----
    if tweaks {
        progress("tweaks: logd stores, keybag, containers, data ark, preferences");
        for (d, mode) in [
            ("/private/var/db/diagnostics", 0o751u16),
            ("/private/var/db/uuidtext", 0o755),
            ("/private/var/db/diagnostics/timesync", 0o755),
        ] {
            b.ensure_dir_path(d, 272, 272, mode)?;
            b.put_created(&format!("{d}/.migrated"), 272, 272, 0o644, &[]);
        }
        let kb = xml_plist("<dict><key>KeyBagKeys</key><data>U1dFRVQ=</data></dict>");
        b.ensure_dir_path("/private/var/keybags", 0, 0, 0o700)?;
        b.put_created("/private/var/keybags/systembag.kb", 0, 0, 0o600, &kb);

        for (d, uid) in [
            ("/private/var/containers/Bundle", 33u32),
            ("/private/var/containers/Data/System", 0),
            ("/private/var/containers/Shared/SystemGroup", 0),
            ("/private/var/containers/Temp", 0),
        ] {
            b.ensure_dir_path(d, uid, uid, 0o755)?;
        }
        // mobileactivationd system container + data ark (Hactivation branch)
        let cbase = format!("/private/var/containers/Data/System/{ACTIVATION_CONTAINER}");
        b.ensure_dir_path(&format!("{cbase}/Library/internal"), MOBILE, MOBILE, 0o755)?;
        let mut meta = BTreeMap::new();
        meta.insert(
            "MCMMetadataIdentifier".to_string(),
            Plist::Str("com.apple.mobileactivationd".into()),
        );
        meta.insert("MCMMetadataStoreClass".to_string(), Plist::Str("System".into()));
        meta.insert("MCMMetadataUsage".to_string(), Plist::Str("System".into()));
        b.put_created(
            &format!("{cbase}/.com.apple.mobile_container_manager.metadata.plist"),
            MOBILE,
            MOBILE,
            0o644,
            &plist_write(&Plist::Dict(meta))?,
        );
        let mut ark = BTreeMap::new();
        ark.insert("-ActivationState".to_string(), Plist::Str("Activated".into()));
        ark.insert("-BrickState".to_string(), Plist::Bool(false));
        ark.insert("-BuildVersion".to_string(), Plist::Str(BUILD.into()));
        ark.insert("-ProductVersion".to_string(), Plist::Str(VERSION.into()));
        ark.insert("-DataMigrationCompleted".to_string(), Plist::Bool(true));
        ark.insert("-PreSydroDataMigrationCompleted".to_string(), Plist::Bool(true));
        b.put_created(
            &format!("{cbase}/Library/internal/data_ark.plist"),
            MOBILE,
            MOBILE,
            0o600,
            &plist_write(&Plist::Dict(ark))?,
        );

        let prefs = "/private/var/mobile/Library/Preferences";
        b.ensure_dir_path(prefs, MOBILE, MOBILE, 0o755)?;
        let mut mig = BTreeMap::new();
        mig.insert("LastSystemVersion".to_string(), Plist::Str(BUILD.into()));
        let mut dd = BTreeMap::new();
        dd.insert("SyntheticDidUpgrade".to_string(), Plist::Bool(false));
        dd.insert("buildVersion".to_string(), Plist::Str(BUILD.into()));
        mig.insert("DMUserDataDisposition".to_string(), Plist::Dict(dd));
        b.put_created(
            &format!("{prefs}/com.apple.migration.plist"),
            MOBILE,
            MOBILE,
            0o600,
            &plist_write(&Plist::Dict(mig))?,
        );
        let mut sb = BTreeMap::new();
        sb.insert("BKNoWatchdogs".to_string(), Plist::Bool(true));
        let mut f = BTreeMap::new();
        f.insert("com.apple.purplebuddy".to_string(), Plist::Real(100.0));
        sb.insert("FBLaunchWatchdogExceptions".to_string(), Plist::Dict(f));
        sb.insert("MCForceShowCloudConfigurationUI".to_string(), Plist::Bool(false));
        b.put_created(
            &format!("{prefs}/com.apple.springboard.plist"),
            MOBILE,
            MOBILE,
            0o600,
            &plist_write(&Plist::Dict(sb))?,
        );
        let mut pb = BTreeMap::new();
        pb.insert("SetupDone".to_string(), Plist::Bool(true));
        pb.insert("SetupFinishedAllSteps".to_string(), Plist::Bool(true));
        pb.insert("SetupVersion".to_string(), Plist::Int(11));
        pb.insert("daemonizeNeedsToRun".to_string(), Plist::Bool(false));
        b.put_created(
            &format!("{prefs}/com.apple.purplebuddy.plist"),
            MOBILE,
            MOBILE,
            0o600,
            &plist_write(&Plist::Dict(pb))?,
        );
        let mut pbnb = BTreeMap::new();
        for k in [
            "ShouldRunDiagnosticsMiniBuddy",
            "ShouldRunHSA2MiniBuddy",
            "ShouldRunKeychainSyncMiniBuddy",
            "ShouldRunMesaMiniBuddy",
            "ShouldRunPaymentMiniBuddy",
            "ShouldRunStandardMiniBuddy",
            "SilentICDPUpgrade",
        ] {
            pbnb.insert(k.to_string(), Plist::Bool(false));
        }
        b.put_created(
            &format!("{prefs}/com.apple.purplebuddy.notbackedup.plist"),
            MOBILE,
            MOBILE,
            0o600,
            &plist_write(&Plist::Dict(pbnb))?,
        );
        let mut mc = BTreeMap::new();
        mc.insert(
            "LockdownActivationIndicatesCloudConfigurationAvailable".to_string(),
            Plist::Bool(false),
        );
        b.put_created(
            &format!("{prefs}/com.apple.managedconfiguration.notbackedup.plist"),
            MOBILE,
            MOBILE,
            0o600,
            &plist_write(&Plist::Dict(mc))?,
        );
    }

    // ---- verify the boot-critical closure BEFORE shipping the image ----
    // (a missing /dev cost a full CI round-trip to discover)
    {
        let cat = b.img.catalog();
        verify_boot_critical(&cat, sysos.is_some(), &|m| progress(m))?;
    }

    // ---- finalize: catalog + bitmap + volume headers ----
    progress("finish: catalog b-tree");
    let report = Report {
        files: b.files,
        dirs: b.dirs,
        bytes: b.bytes,
        skipped: b.skipped,
        image_blocks: total_blocks,
    };
    b.img.finish("iOSRoot")?;
    Ok(report)
}

fn split_parent(path: &str) -> (String, &str) {
    match path.rfind('/') {
        Some(0) | None => ("/".to_string(), &path[1..]),
        Some(i) => (path[..i].to_string(), &path[i + 1..]),
    }
}

/// Assert the paths the boot needs (see docs/research/userspace-deps.md and
/// xnu bsd_init.c / launchd): devfs mountpoint, init program, daemon plists,
/// SystemVersion, dyld shared cache when a cryptex source was given.
fn verify_boot_critical(
    cat: &[crate::hfs::CatEntry],
    sysos: bool,
    progress: &dyn Fn(&str),
) -> Result<()> {
    use crate::hfs::CatEntry;
    let find = |p: &str| cat.iter().find(|e: &&CatEntry| e.path == p);
    let mut missing: Vec<String> = Vec::new();
    for d in [
        "/dev",
        "/mnt",
        "/private/var",
        "/private/etc",
        "/sbin",
        "/usr/lib",
        "/usr/libexec",
        "/System/Library/LaunchDaemons",
        "/System/Library/CoreServices",
        "/System/Library/xpc",
    ] {
        match find(d) {
            Some(e) if e.is_dir => {}
            Some(_) => missing.push(format!("{d} (exists but is NOT a dir)")),
            None => missing.push(format!("{d} (missing dir)")),
        }
    }
    for f in ["/sbin/launchd", "/System/Library/CoreServices/SystemVersion.plist"] {
        if !matches!(find(f), Some(e) if !e.is_dir && e.size > 0) {
            missing.push(format!("{f} (missing file)"));
        }
    }
    let ndaemons = cat
        .iter()
        .filter(|e| !e.is_dir && e.path.starts_with("/System/Library/LaunchDaemons/"))
        .count();
    if ndaemons < 100 {
        missing.push(format!(
            "/System/Library/LaunchDaemons plists ({ndaemons} < 100)"
        ));
    }
    if sysos {
        let dsc = cat
            .iter()
            .any(|e| {
                !e.is_dir
                    && e.path.starts_with("/System/Library/Caches/com.apple.dyld/dyld_shared_cache")
            });
        if !dsc {
            missing.push(
                "/System/Library/Caches/com.apple.dyld/dyld_shared_cache* (missing DSC)".into(),
            );
        }
    }
    if missing.is_empty() {
        progress(&format!(
            "verify: boot-critical closure OK (dev dir, launchd, {ndaemons} daemons, CoreServices, DSC={})",
            sysos
        ));
        Ok(())
    } else {
        Err(crate::Error::Msg(format!(
            "boot-critical closure INCOMPLETE: {}",
            missing.join(", ")
        )))
    }
}
