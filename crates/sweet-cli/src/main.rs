//! sweet-rust — the ios-sweet boot pipeline, in Rust.
//!
//! One binary, std-only + vendored native C (Apple LZFSE, miniz):
//!   ipsw-info | ipsw-extract | ipsw-ls | ipsw-find | ipsw-file
//!   kernel-unpack | make-adt | make-payload | make-bootimg
//!   logo-extract | hello-extract | fb-render | inject-logo | demo-kernel
//!   boot-qemu | ipsw-boot | emu-boot (live control) | emu-peek

use std::path::{Path, PathBuf};
use std::process::exit;

use sweet_core as sc;
use sc::adt;
use sc::bootimg;
use sc::cpio;
use sc::demo;
use sc::fblogo;
use sc::im4p;
use sc::ipsw;
use sc::mkadt::{self, AdtOptions};
use sc::payload;
use sc::qemu::{self, J, RunOptions};
use sc::{bail, FB_H, FB_W, Result};

const HELP: &str = "\
sweet-rust — boot the Apple logo from an IPSW (ios-sweet pipeline, in Rust)

USAGE:
    sweet-rust <COMMAND> [ARGS]

COMMANDS:
    ipsw-info <IPSW|URL>
        List the IPSW zip members (ZIP64 aware; URL uses curl range requests).

    ipsw-extract <IPSW|URL> --member NAME [--member NAME ...] --out DIR
        Extract members (exact name or substring match).

    ipsw-ls <IPSW|URL|DMG> [PATH]
        List a directory of the IPSW system volume (default /).

    ipsw-find <IPSW|URL|DMG> <SUBSTR>
        Case-insensitive path search across the whole system volume.

    ipsw-file <IPSW|URL|DMG> <PATH> --out FILE
        Extract a real file from the system volume (decmpfs-decoded).

    macho-deps <FILE>
        Parse the arm64 slice of a (FAT or thin) Mach-O: cputype, segments,
        and every LC_LOAD_DYLIB / LC_LOAD_WEAK_DYLIB / LC_REEXPORT_DYLIB.

    kc-kexts <KERNELCACHE.macho> [--out FILE.md]
        Parse the prelinked kernelcache __PRELINK_INFO plist: every kext
        (bundle id, path, load addr) + IOKitPersonalities (IONameMatch,
        IOProviderClass) - the driver-vs-hardware matching table.

    adt-dump <DeviceTree.im4p> [--out FILE.md] [--table]
        Parse the real ADT from an IPSW DeviceTree im4p: full tree dump,
        optionally one line per device node (path, compatible, reg base/size).

    emu-boot --ipsw IPSW|URL <OUTDIR> [--wallpaper PNG] [--send TEXT]
        Boot into a LIVE controllable session: QMP + gdbstub + serial socket.
        The host pauses the VM, repaints the framebuffer with the wallpaper,
        types into the guest UART and screenshots every stage.

    emu-peek <PAYLOAD> <OUTDIR> [--addr HEX] [--len N] [--secs 6]
        Boot a payload, pause the live VM and hex-dump guest RAM (gdbstub).

    kernel-unpack <IN.im4p> <OUT.macho>
        IM4P unwrap + LZFSE (bvx2) decompress -> plain Mach-O kernel.

    make-adt <OUT.adt> [--qemu] [--highmem] [--product FILE.adt] [--seed HEX] [--dump]
        Build the Apple Device Tree (mkadt parity). --product copies the
        /product node of the real D22AP tree (MobileGestalt identity).

    make-payload <KERNEL> <ADT> <OUT> [--cmdline \"...\"] [--ramdisk IMAGE]
        Build the SWEETXNU payload (walks LC_DYLD_CHAINED_FIXUPS; the ramdisk
        is registered as /chosen/memory-map/RAMDisk -> md0).

    make-bootimg --kernel Image --ramdisk R [--dtb DTB] [--cmdline S] [-o OUT]
        Android boot image v2 (shim/build.sh mkbootimg parity).

    logo-extract <IPSW|URL> <OUT.png>
        Pull the Apple logo asset out of the IPSW and save it as PNG.

    fb-render <IPSW|URL> <OUT.png> [--w 1080 --h 2400] [--logo-scale 33]
        Compose the IPSW Apple logo onto a black BGRA framebuffer -> PNG.
        This is the no-QEMU proof of the \"Apple logo from IPSW\" path.

    inject-logo <IPSW|URL> <OUT.logo.bin> [--w --h]
        Write the BGRA framebuffer blob for QEMU's loader device
        (-device loader,file=...,addr=0x9c000000,force-raw=on).

    demo-kernel <OUT.macho> [--color 0x30c040] [--strip 400]
        Synthesize a bootable arm64 demo kernel (bottom-strip painter +
        PL011 serial marker) for QEMU/CI when no SWEET kernel is at hand.

    place-image <PAYLOAD> <OUT.bin>
        Write the physical RAM image (segments + ADT + boot_args) without
        running QEMU - for debugging loaders.

    boot-qemu <PAYLOAD> <OUTDIR> [--secs 20] [--cpu cortex-a76] [--gdb]
        [--logo LOGO.bin | --logo-ipsw IPSW] [--live-serial] [--qemu-datadir D]
        [--check-env]
        Boot the payload in QEMU virt; writes fb.png / regs.txt / serial.txt /
        qemu.log / result.json (qemu-sweet.py parity, exit codes 0/1/2/3/130).

    ipsw-boot --ipsw IPSW|URL <OUTDIR> [--kernel K | --demo] [--ramdisk IMAGE]
        [--cmdline \"...\"] [--secs 120] [--product FILE.adt] [--logo-scale 33]
        [--cpu MODEL] [--no-logo] [--live-serial]
        The full pipeline in one command: IPSW -> logo asset + (kernelcache |
        demo kernel) -> payload -> logo injection -> QEMU -> fb.png +
        boot-verdict.json. Verdict checks: logo visible, serial marker,
        panic watch.

    --help | -h       this text
    --version         print version

EXAMPLES:
    sweet-rust fb-render iPhone10,3_16.7.16_20H392_Restore.ipsw fb.png
    sweet-rust ipsw-boot --ipsw https://...ipsw out/ --demo --secs 30
    sweet-rust ipsw-boot --ipsw IPSW out/ --kernel kernel.development.sweet \\
        --ramdisk init --cmdline 'debug=0xc serial=3 cpus=1 rd=md0' --secs 120
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") || args.is_empty() {
        print!("{HELP}");
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("sweet-rust {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let code = run(&args);
    exit(code);
}

fn run(args: &[String]) -> i32 {
    match dispatch(args) {
        Ok(()) => 0,
        Err(sc::Error::Msg(m)) if m.starts_with("EXIT1:") => {
            eprintln!("{m}");
            1
        }
        Err(sc::Error::Msg(m)) if m.starts_with("EXIT2:") => {
            eprintln!("{m}");
            2
        }
        Err(sc::Error::Msg(m)) if m.starts_with("EXIT3:") => {
            eprintln!("{m}");
            3
        }
        Err(e) => {
            eprintln!("[!] error: {e}");
            1
        }
    }
}

struct Flags<'a> {
    map: std::collections::HashMap<&'a str, String>,
    rest: Vec<&'a str>,
}

impl<'a> Flags<'a> {
    fn parse(args: &'a [String], value_flags: &[&str]) -> Flags<'a> {
        let mut map = std::collections::HashMap::new();
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if let Some(name) = a.strip_prefix("--") {
                if value_flags.contains(&name) && i + 1 < args.len() {
                    map.insert(name, args[i + 1].clone());
                    i += 2;
                    continue;
                }
                map.insert(name, String::new());
            } else {
                rest.push(a);
            }
            i += 1;
        }
        Flags { map, rest }
    }

    fn has(&self, k: &str) -> bool {
        self.map.contains_key(k)
    }

    fn val(&self, k: &str) -> Option<&str> {
        self.map.get(k).map(|s| s.as_str())
    }

    fn num(&self, k: &str, default: u64) -> u64 {
        self.val(k).and_then(|v| {
            let v = v.trim_start_matches("0x");
            u64::from_str_radix(v, if v.len() > 0 && self.val(k).unwrap().starts_with("0x") { 16 } else { 10 })
                .ok()
        }).unwrap_or(default)
    }
}

fn dispatch(args: &[String]) -> Result<()> {
    let cmd = args[0].as_str();
    let rest = &args[1..];
    match cmd {
        "ipsw-info" => cmd_ipsw_info(rest),
        "ipsw-extract" => cmd_ipsw_extract(rest),
        "ipsw-ls" => cmd_ipsw_ls(rest),
        "ipsw-find" => cmd_ipsw_find(rest),
        "ipsw-file" => cmd_ipsw_file(rest),
        "ipsw-stat" => cmd_ipsw_stat(rest),
        "rootfs-plan" => cmd_rootfs_plan(rest),
        "mkiosroot" => cmd_mkiosroot(rest),
        "screen-decode" => cmd_screen_decode(rest),
        "macho-deps" => cmd_macho_deps(rest),
        "kc-kexts" => cmd_kc_kexts(rest),
        "adt-dump" => cmd_adt_dump(rest),
        "emu-boot" => cmd_emu_boot(rest),
        "emu-peek" => cmd_emu_peek(rest),
        "kernel-unpack" => cmd_kernel_unpack(rest),
        "make-adt" => cmd_make_adt(rest),
        "make-payload" => cmd_make_payload(rest),
        "make-bootimg" => cmd_make_bootimg(rest),
        "logo-extract" => cmd_logo_extract(rest),
        "hello-extract" => cmd_hello_extract(rest),
        "fb-render" => cmd_fb_render(rest),
        "inject-logo" => cmd_inject_logo(rest),
        "demo-kernel" => cmd_demo_kernel(rest),
        "boot-qemu" => cmd_boot_qemu(rest),
        "place-image" => cmd_place_image(rest),
        "ipsw-boot" => cmd_ipsw_boot(rest),
        other => {
            eprintln!("unknown command {other:?} (see --help)");
            bail!("unknown command");
        }
    }
}

fn value_flags(cmd: &str) -> Vec<&'static str> {
    match cmd {
        "ipsw-extract" => vec!["member", "out"],
        "ipsw-file" => vec!["out"],
        "macho-deps" => vec![],
        "kc-kexts" => vec!["out"],
        "adt-dump" => vec!["out"],
        "emu-boot" => vec![
            "ipsw", "wallpaper", "wallpaper-manifest", "send", "secs", "cpu",
            "strip", "logo-scale", "qemu-datadir",
        ],
        "emu-peek" => vec!["addr", "len", "secs", "cpu"],
        "kernel-unpack" => vec![],
        "make-adt" => vec!["product", "seed", "phys", "mem"],
        "make-payload" => vec!["cmdline", "ramdisk"],
        "make-bootimg" => vec!["kernel", "ramdisk", "dtb", "cmdline", "o"],
        "logo-extract" | "inject-logo" => vec![],
        "fb-render" => vec!["w", "h", "logo-scale"],
        "demo-kernel" => vec!["color", "strip", "png"],
        "hello-extract" => vec!["lang", "ipsw"],
        "mkiosroot" => vec!["os", "systemos", "appos", "profile", "out"],
        "rootfs-plan" => vec![],
        "screen-decode" => vec!["kernel", "payload", "nm"],
        "boot-qemu" => vec!["secs", "cpu", "logo", "logo-ipsw", "qemu-datadir", "hmp"],
        "ipsw-boot" => vec![
            "ipsw", "kernel", "ramdisk", "cmdline", "secs", "product",
            "logo-scale", "cpu", "w", "h", "qemu-datadir", "hello-lang",
        ],
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// IM4P/ADT blob loader: raw file or IM4P-wrapped (DeviceTree im4p).
fn load_adtish(path: &Path) -> Result<Vec<u8>> {
    let buf = std::fs::read(path)?;
    if im4p::looks_im4p(&buf) {
        let (p, _) = im4p::unwrap(&buf)?;
        Ok(p)
    } else {
        Ok(buf)
    }
}

/// The Apple logo asset inside an IPSW: the all_flash member whose name
/// contains "applelogo" (iPhone10,3 16.7.16: applelogo@3x~iphone.im4p).
fn find_logo<'a>(idx: &'a ipsw::IpswIndex) -> Result<&'a ipsw::IpswEntry> {
    idx.entries
        .iter()
        .filter(|e| e.name.to_ascii_lowercase().contains("applelogo"))
        .max_by_key(|e| e.size)
        .ok_or_else(|| sc::Error::Msg("no applelogo member in the IPSW".into()))
}

/// Extract + decode the Apple logo of an IPSW (local file or URL) to RGBA.
/// Locate the zip-STORED rootfs dmg member (largest .dmg, method 0).
fn rootfs_member(source: &str) -> Result<String> {
    let idx = ipsw::index(source)?;
    let best = idx
        .entries
        .iter()
        .filter(|e| e.name.ends_with(".dmg") && e.method == 0 && e.size > (1 << 30))
        .max_by_key(|e| e.size)
        .ok_or_else(|| sc::Error::Msg("no stored rootfs .dmg member in IPSW".into()))?;
    Ok(best.name.clone())
}

/// Open the IPSW system volume: zip-STORED dmg member (or a raw local dmg
/// file) -> UDIF blkx chunks (lzfse/zlib via vendored Apple code) -> APFS
/// catalog walk. Returns the provenance member name and the indexed volume.
fn open_system_volume(source: &str) -> Result<(String, sc::apfs::Apfs)> {
    // raw local dmg (koly trailer, no zip EOCD) vs full IPSW zip
    let (member, boxed, size): (String, Box<dyn sc::dmg::ByteSource>, u64) =
        if !source.starts_with("http") && local_is_raw_dmg(source)? {
            let f = std::fs::File::open(source)?;
            let size = f.metadata()?.len();
            let member = std::path::Path::new(source)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "rootfs.dmg".into());
            (member, Box::new(sc::dmg::FileSource::open(std::path::Path::new(source))?), size)
        } else {
            let m = rootfs_member(source)?;
            let ms = sc::dmg::MemberSource::open(source, &m)?;
            let size = ms.size;
            (m, Box::new(ms) as Box<dyn sc::dmg::ByteSource>, size)
        };
    let dmg = sc::dmg::Dmg::open_auto(boxed, size)?;
    let vol = sc::apfs::Apfs::open(dmg)?;
    Ok((member, vol))
}

/// Extract the real CursiveHello.json for `lang` from the IPSW system volume.
fn hello_json_from_ipsw(source: &str, lang: &str) -> Result<(Vec<u8>, String, String)> {
    let (member, mut vol) = open_system_volume(source)?;
    let path = format!(
        "/System/Library/PrivateFrameworks/SpringBoard.framework/{lang}.lproj/CursiveHello.json"
    );
    let fid = vol.resolve(&path)?;
    let json = vol.read_file(fid)?;
    let sha = sc::hash::hex(&sc::hash::sha256(&json));
    Ok((json, format!("{member}:{path}"), sha))
}

/// ipsw-ls: list a directory of the system volume.
fn cmd_ipsw_ls(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("ipsw-ls"));
    let Some(src) = f.rest.first() else {
        bail!("usage: ipsw-ls <IPSW|URL|DMG> [PATH]");
    };
    let path = f.rest.get(1).copied().unwrap_or("/");
    let (_member, vol) = open_system_volume(src)?;
    let dir = if path == "/" { 2u64 } else { vol.resolve(path)? };
    if !vol.list_dir(dir).is_empty() || path == "/" {
        for (name, fid, is_dir) in vol.list_dir(dir) {
            if is_dir {
                println!("d {name}/");
            } else {
                let size = vol.stat(fid).map(|(.., s)| s).unwrap_or(0);
                println!("  {name}  ({size} bytes, id {fid})");
            }
        }
    } else {
        let (mode, .., size) = vol.stat(dir).unwrap_or((0, 0, 0, 0));
        println!("{path}: not a directory (mode {mode:#o}, {size} bytes)");
    }
    Ok(())
}

/// ipsw-find: case-insensitive substring search across the system volume.
fn cmd_ipsw_find(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("ipsw-find"));
    let [src, needle] = f.rest.as_slice() else {
        bail!("usage: ipsw-find <IPSW|URL|DMG> <SUBSTR>");
    };
    let needle_l = needle.to_lowercase();
    let (_member, vol) = open_system_volume(src)?;
    let mut hits = 0;
    for (path, fid) in vol.walk_all() {
        if path.to_lowercase().contains(&needle_l) {
            let size = vol.stat(fid).map(|(.., s)| s).unwrap_or(0);
            println!("{path}  ({size} bytes)");
            hits += 1;
        }
    }
    if hits == 0 {
        eprintln!("no match for {needle:?}");
    }
    Ok(())
}

/// ipsw-file: extract a real file from the system volume.
fn cmd_ipsw_file(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("ipsw-file"));
    let Some(src) = f.rest.first() else {
        bail!("usage: ipsw-file <IPSW|URL|DMG> <PATH> --out FILE");
    };
    let Some(path) = f.rest.get(1) else {
        bail!("usage: ipsw-file <IPSW|URL|DMG> <PATH> --out FILE");
    };
    let Some(out) = f.val("out") else {
        bail!("--out FILE required");
    };
    let (member, mut vol) = open_system_volume(src)?;
    let fid = vol.resolve(path)?;
    let bytes = vol.read_file(fid)?;
    std::fs::write(out, &bytes)?;
    let sha = sc::hash::hex(&sc::hash::sha256(&bytes));
    println!(
        "[ok] {path} <- {member}: {} bytes, sha256 {sha}",
        bytes.len()
    );
    Ok(())
}

/// rootfs-plan: walk the OS volume and print the full-parity size plan
/// (file/dir counts, uncompressed bytes, block-rounded data, catalog
/// estimate) — the input for mkiosroot sizing.
fn cmd_rootfs_plan(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("rootfs-plan"));
    let Some(src) = f.rest.first() else { bail!("usage: rootfs-plan <IPSW|URL|DMG>") };
    let (_member, mut vol) = open_system_volume(src)?;
    let mut files = 0u64;
    let mut dirs = 0u64;
    let mut bytes = 0u64;
    let mut rounded = 0u64;
    let mut compressed = 0u64;
    for (path, fid) in vol.walk_all() {
        let is_dir = !vol.list_dir(fid).is_empty();
        if is_dir {
            dirs += 1;
            continue;
        }
        if let Some((mode, _uid, _gid, size)) = vol.stat(fid) {
            files += 1;
            bytes += size;
            rounded += size.div_ceil(4096) * 4096;
            if vol.xattr(fid, "com.apple.decmpfs").is_some() {
                compressed += 1;
            }
        }
        let _ = path;
    }
    println!(
        "OS volume plan: {files} files, {dirs} dirs, uncompressed {bytes} ({} GiB), block-rounded {rounded} ({} GiB), decmpfs-compressed {compressed}",
        bytes / 2 ^ 30.min(1),
        rounded / 2 ^ 30.min(1)
    );
    Ok(())
}

/// mkiosroot: build the real iOS HFS+ root image (root-free; the pure-Rust
/// parity of upstream mkiosroot.sh + sweet-root-tweaks.py).
///
///   mkiosroot --os <IPSW|URL|DMG> --out OUT.img [--systemos FILE]
///             [--appos FILE] [--profile mini|full] [--no-tweaks]
fn cmd_mkiosroot(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("mkiosroot"));
    let Some(os_src) = f.val("os") else { bail!("--os <IPSW|URL|DMG> required") };
    let Some(out) = f.val("out") else { bail!("--out OUT.img required") };
    let profile = f.val("profile").unwrap_or_else(|| "mini".into());
    let tweaks = !f.has("no-tweaks");
    if profile != "mini" && profile != "full" {
        bail!("--profile must be mini or full");
    }
    let (_member, mut os) = open_system_volume(&os_src)?;
    let mut sysos = match f.val("systemos") {
        Some(p) => Some(sc::apfs::Apfs::open(sc::dmg::Dmg::open_auto(
            Box::new(sc::dmg::FileSource::open(std::path::Path::new(&p))?),
            std::fs::metadata(&p)?.len(),
        )?)?),
        None => None,
    };
    let mut appos = match f.val("appos") {
        Some(p) => Some(sc::apfs::Apfs::open(sc::dmg::Dmg::open_auto(
            Box::new(sc::dmg::FileSource::open(std::path::Path::new(&p))?),
            std::fs::metadata(&p)?.len(),
        )?)?),
        None => None,
    };
    let t0 = std::time::Instant::now();
    let unlink = if f.has("unlink-systemos") {
        f.val("systemos").map(std::path::PathBuf::from)
    } else {
        None
    };
    let sys_ref = sysos
        .as_mut()
        .map(|v| (&mut *v, unlink));
    let app_ref = appos.as_mut();
    let report = sc::rootfs::build(
        &mut os,
        sys_ref,
        app_ref,
        std::path::Path::new(&out),
        &profile,
        tweaks,
        &|msg| eprintln!("[mkiosroot] {msg}"),
    )?;
    let sha = {
        use std::io::Read;
        let mut h = sc::hash::Sha256::new();
        let mut fh = std::fs::File::open(&out)?;
        let mut buf = vec![0u8; 4 << 20];
        loop {
            let n = fh.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        sc::hash::hex(&h.finalize())
    };
    println!(
        "[ok] {out}: profile={profile} files={} dirs={} data={:.2} GiB skipped={} blocks={} time={:?} sha256 {sha}",
        report.files,
        report.dirs,
        report.bytes as f64 / (1 << 30) as f64,
        report.skipped,
        report.image_blocks,
        t0.elapsed()
    );
    Ok(())
}

/// screen-decode: turn a booted fb.png back into console text using the
/// kernel's _iso_font glyph table (exact decode_screen port; needs llvm-nm
/// on the unstripped kernel + the payload.bin that was booted).
fn cmd_screen_decode(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("screen-decode"));
    let Some(png_path) = f.rest.first() else {
        bail!("usage: screen-decode --kernel K --payload P <fb.png> [--nm llvm-nm]");
    };
    let kernel = f.val("kernel").ok_or_else(|| sc::Error::Msg("--kernel required".into()))?;
    let payload = f.val("payload").ok_or_else(|| sc::Error::Msg("--payload required".into()))?;
    let nm = f.val("nm").unwrap_or_else(|| "llvm-nm".into());
    let font = sc::qemu::extract_font(
        std::path::Path::new(&kernel),
        &std::fs::read(payload)?,
        &nm,
    )?;
    let img = sc::png::decode(&std::fs::read(png_path)?)?;
    let mut raw = vec![0u8; (FB_STRIDE_US * FB_H) as usize];
    for y in 0..img.height.min(FB_H) {
        for x in 0..img.width.min(FB_W) {
            let [r, g, b, a] = img.pixel(x, y);
            let o = (y as usize * FB_STRIDE_US as usize) + x as usize * 4;
            raw[o] = b;
            raw[o + 1] = g;
            raw[o + 2] = r;
            raw[o + 3] = a;
        }
    }
    let (screen, unknown) = sc::qemu::decode_screen(&raw, &font);
    print!("{screen}");
    eprintln!("[screen-decode] unknown glyphs: {unknown}");
    Ok(())
}

/// ipsw-stat: diagnostics for one path (fid, mode/uid/gid/size, inode val
/// length, extents, decmpfs presence) — used to debug rootfs assembly.
fn cmd_ipsw_stat(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("ipsw-stat"));
    let Some(src) = f.rest.first() else { bail!("usage: ipsw-stat <IPSW|URL|DMG> <PATH>") };
    let Some(path) = f.rest.get(1) else { bail!("usage: ipsw-stat <IPSW|URL|DMG> <PATH>") };
    let (member, mut vol) = open_system_volume(src)?;
    let fid = vol.resolve(path)?;
    let st = vol.stat(fid);
    println!("path {path} <- {member}");
    println!("fid {fid} stat {st:?}");
    println!("inode_val_len {:?}", vol.inode_val_len(fid));
    let xs = vol.xattr(fid, "com.apple.decmpfs").is_some();
    println!("decmpfs: {xs}");
    println!("xattrs: {:?}", vol.xattr_names(fid));
    let exts = vol.extent_list(fid);
    println!("extents: {} (covered {} bytes)", exts.len(), exts.iter().map(|e| e.2).sum::<u64>());
    for (i, (la, pb, ln)) in exts.iter().take(4).enumerate() {
        println!("  ext{i}: laddr={la} pbn={pb} len={ln}");
    }
    if let Some(dec) = vol.xattr(fid, "com.apple.decmpfs") {
        if dec.len() >= 20 {
            let algo = u32::from_le_bytes([dec[8], dec[9], dec[10], dec[11]]);
            if algo == 4 || algo == 12 {
                if let Some(r) = vol.xattr(fid, "com.apple.ResourceFork") {
                    if r.len() >= 20 {
                        let oid = u64::from_le_bytes(r[4..12].try_into().unwrap()) & 0x0FFF_FFFF_FFFF_FFFF;
                        let exts = vol.extent_list(oid);
                        println!("xattr oid {oid:#x}: {} extents", exts.len());
                        for (i, (la, pb, ln)) in exts.iter().enumerate() {
                            println!("  ext{i}: laddr={la} pbn={pb} len={ln}");
                        }
                    }
                }
            }
        }
    }
    for n in vol.xattr_names(fid) {
        if n == "com.apple.ResourceFork" || n == "com.apple.fs.symlink" {
            if let Some(v) = vol.xattr(fid, &n) {
                println!("  {n}: {} bytes head={:02x?}", v.len(), &v[..v.len().min(24)]);
            }
        }
    }
    if let Some(dec) = vol.xattr(fid, "com.apple.decmpfs") {
        println!(
            "decmpfs hdr: flags={:#x} len={} magic={:?} algo={} usize={}",
            u16::from_le_bytes([dec[0], dec[1]]),
            u16::from_le_bytes([dec[2], dec[3]]),
            &dec[4..8],
            u32::from_le_bytes([dec[8], dec[9], dec[10], dec[11]]),
            u64::from_le_bytes(dec[12..20].try_into().unwrap())
        );
        println!("cmpf payload {} bytes", dec.len() - 4 - u16::from_le_bytes([dec[2], dec[3]]) as usize + 0);
    }
    if std::env::var("SWEET_INODE_DUMP").is_ok() {
        if let Some(raw) = vol.inode_raw(fid) {
        println!("inode_val ({} bytes):", raw.len());
        for (i, b) in raw.chunks(8).enumerate() {
            let mut hexs = String::new();
            for x in b {
                hexs += &format!("{x:02x} ");
            }
            let mut le = String::new();
            if b.len() == 8 {
                let v = u64::from_le_bytes(b.try_into().unwrap());
                le = format!("u64={v} ({v:#x})");
            }
            println!("  +{:03}: {hexs} {le}", i * 8);
        }
        }
    }
    Ok(())
}

/// macho-deps: cputype, segments and dylibs of the arm64 slice of a Mach-O.
fn cmd_macho_deps(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("macho-deps"));
    let Some(path) = f.rest.first() else {
        bail!("usage: macho-deps <MACHO>");
    };
    let buf = std::fs::read(path)?;
    let (m, slice_off) = sc::macho::parse_best(&buf)?;
    println!("file        : {path} ({} bytes)", buf.len());
    println!("arm64 slice : offset {slice_off:#x}");
    println!("cputype     : {:#x}", m.cputype);
    println!("entry       : {:#x}", m.entry);
    println!("segments ({}):", m.segments.len());
    for s in &m.segments {
        println!(
            "  {:<18} vm {:#011x} vsize {:#x} file {:#x} fsize {}",
            s.name, s.vmaddr, s.vmsize, s.fileoff, s.filesize
        );
    }
    println!("dylibs ({}):", m.dylibs.len());
    for d in &m.dylibs {
        println!("  {d}");
    }
    Ok(())
}

/// kc-kexts: kext + IOKit personality inventory of a prelinked kernelcache.
fn cmd_kc_kexts(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("kc-kexts"));
    let Some(path) = f.rest.first() else {
        bail!("usage: kc-kexts <KERNELCACHE.macho> [--out FILE.md]");
    };
    let buf = std::fs::read(path)?;
    let mut kexts = sc::kexts::prelink_kexts(&buf)?;
    kexts.sort_by(|a, b| a.bundle_id.cmp(&b.bundle_id));
    let nper: usize = kexts.iter().map(|k| k.personalities.len()).sum();
    println!("kexts            : {}", kexts.len());
    println!("personalities    : {nper}");
    let with_match: usize =
        kexts.iter().filter(|k| k.personalities.iter().any(|p| !p.io_name_match.is_empty())).count();
    println!("with IONameMatch : {with_match}");
    let mut md = String::new();
    md.push_str(&format!(
        "# Kernelcache kext inventory (real IPSW)\n\n| # | CFBundleIdentifier | bundle path | personalities | IONameMatch |\n|---|---|---|---|---|\n"
    ));
    for (i, k) in kexts.iter().enumerate() {
        let per: Vec<String> = k
            .personalities
            .iter()
            .map(|p| {
                let m = if p.io_name_match.is_empty() {
                    p.io_provider_class.clone().unwrap_or_default()
                } else {
                    p.io_name_match.join(" / ")
                };
                format!("{}: {}", p.name, m)
            })
            .collect();
        let per = if per.is_empty() { "-".into() } else { per.join("<br>") };
        println!("{:3} {:<58} {}", i + 1, k.bundle_id, k.path);
        md.push_str(&format!(
            "| {} | `{}` | `{}` | {} | {} |\n",
            i + 1,
            k.bundle_id,
            k.path,
            k.personalities.len(),
            per.replace('|', "\\|")
        ));
    }
    if let Some(out) = f.val("out") {
        std::fs::write(out, &md)?;
        eprintln!("[ok] wrote {out}");
    }
    Ok(())
}

/// adt-dump: parse the real ADT out of a DeviceTree im4p and dump it.
fn cmd_adt_dump(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("adt-dump"));
    let Some(path) = f.rest.first() else {
        bail!("usage: adt-dump <DeviceTree.im4p> [--out FILE] [--table]");
    };
    let raw = load_adtish(Path::new(path))?;
    let (root, _) = adt::parse(&raw)?;
    print!("{}", adt::dump(&root, 0, false));
    if f.has("table") {
        let mut rows = Vec::new();
        adt_walk(&root, "", &mut rows);
        println!("\n--- devices with reg (path | compatible | base size) ---");
        for (p, compat, regs) in &rows {
            if regs.is_empty() {
                continue;
            }
            let rs: Vec<String> =
                regs.iter().map(|(a, s)| format!("{a:#x} {s:#x}")).collect();
            println!("{p:<44} | {compat:<30} | {}", rs.join(" "));
        }
    }
    if let Some(out) = f.val("out") {
        let mut text = adt::dump(&root, 0, false);
        if f.has("table") {
            text.push_str("\n--- devices with reg (path | compatible | base size) ---\n");
            let mut rows = Vec::new();
            adt_walk(&root, "", &mut rows);
            for (p, compat, regs) in rows {
                if regs.is_empty() {
                    continue;
                }
                let rs: Vec<String> =
                    regs.iter().map(|(a, s)| format!("{a:#x} {s:#x}")).collect();
                text.push_str(&format!("{p:<44} | {compat:<30} | {}\n", rs.join(" ")));
            }
        }
        std::fs::write(out, text)?;
        eprintln!("[ok] wrote {out}");
    }
    Ok(())
}

/// (path, compatible, (base,size)[]) for every ADT node carrying a reg.
fn adt_walk(node: &adt::Node, path: &str, out: &mut Vec<(String, String, Vec<(u64, u64)>)>) {
    let p =
        if path.is_empty() { node.name().to_string() } else { format!("{path}/{}", node.name()) };
    let compat = node
        .prop("compatible")
        .map(|v| {
            let s: String = v
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as char)
                .collect();
            s.trim_end_matches('\u{0}').to_string()
        })
        .unwrap_or_default();
    let mut regs = Vec::new();
    if let Some(r) = node.prop("reg") {
        if r.len() >= 16 && r.len() % 16 == 0 {
            for i in 0..r.len() / 16 {
                let a = sc::aarch64::u64le(r, i * 16);
                let s = sc::aarch64::u64le(r, i * 16 + 8);
                regs.push((a, s));
            }
        }
    }
    out.push((p.clone(), compat, regs));
    for c in &node.children {
        adt_walk(c, &p, out);
    }
}

/// Mean absolute byte difference between two equally sized FB dumps.
fn fb_mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return f64::INFINITY;
    }
    let n = a.len().min(b.len());
    let step = 4 * 97; // sparse sample: every 97th pixel, all channels
    let mut acc: u64 = 0;
    let mut cnt: u64 = 0;
    let mut i = 0usize;
    while i + 3 < n {
        acc += (a[i].abs_diff(b[i])
            + a[i + 1].abs_diff(b[i + 1])
            + a[i + 2].abs_diff(b[i + 2])
            + a[i + 3].abs_diff(b[i + 3])) as u64;
        cnt += 4;
        i += step;
    }
    if cnt == 0 { f64::INFINITY } else { acc as f64 / cnt as f64 }
}

/// Count strongly-differing pixels between two FB dumps within a row range.
fn fb_diff_pixels(a: &[u8], b: &[u8], row_from: u32, row_to: u32) -> u64 {
    let stride = FB_STRIDE_US as usize;
    let mut count = 0u64;
    let from = (row_from as usize).min(crate::FB_H as usize);
    let to = (row_to as usize).min(crate::FB_H as usize);
    for y in from..to {
        let base = y * stride;
        for x in 0..(crate::FB_W as usize) {
            let o = base + x * 4;
            if o + 3 >= a.len() || o + 3 >= b.len() {
                break;
            }
            let d = a[o].abs_diff(b[o]) as u64
                + a[o + 1].abs_diff(b[o + 1]) as u64
                + a[o + 2].abs_diff(b[o + 2]) as u64;
            if d > 96 {
                count += 1;
            }
        }
    }
    count
}

/// emu-boot: boot into a LIVE controllable QEMU session (QMP + gdbstub +
/// serial socket) and drive it from the host: screenshot the logo state,
/// repaint the framebuffer with the real IPSW wallpaper at runtime, type
/// into the guest UART and screenshot the input trail.
fn cmd_emu_boot(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("emu-boot"));
    if f.rest.is_empty() {
        bail!("usage: emu-boot --ipsw IPSW|URL <OUTDIR> [--wallpaper PNG] [--send TEXT] [--secs 60]");
    }
    let ipsw_src = f.val("ipsw").ok_or_else(|| sc::Error::Msg("--ipsw required".into()))?;
    let outdir = PathBuf::from(f.rest[0]);
    std::fs::create_dir_all(&outdir)?;
    let secs: f64 = f.val("secs").and_then(|v| v.parse().ok()).unwrap_or(60.0);
    let strip_rows: u32 = f.num("strip", 60) as u32;
    let send_text = f.val("send").unwrap_or("hello from the host").to_string();

    let mut verdict = J::obj();
    verdict.set("ipsw", J::s(ipsw_src));
    verdict.set("mode", J::s("interactive-control"));

    // 1. Apple logo pre-fill (real IPSW asset, same as ipsw-boot)
    let logo_scale = f.num("logo-scale", 33) as u32;
    let mut logo_blob = None;
    match logo_image(ipsw_src) {
        Ok((img, name)) => {
            eprintln!("logo from IPSW: {name} ({}x{})", img.width, img.height);
            verdict.set("logo_member", J::s(name));
            logo_blob = Some(fblogo::loader_blob(&fblogo::compose(&img, logo_scale))?);
        }
        Err(e) => {
            eprintln!("[!] logo extraction failed: {e}");
            verdict.set("logo_member", J::Null);
        }
    }

    // 2. Wallpaper (real IPSW pixels, pre-composited to exactly FB size)
    let mut wall_bgra = None;
    if let Some(wp) = f.val("wallpaper") {
        let img = sc::png::decode(&std::fs::read(wp)?)?;
        if img.width != FB_W || img.height != FB_H {
            bail!(
                "wallpaper PNG must be exactly {FB_W}x{FB_H} (got {}x{})",
                img.width,
                img.height
            );
        }
        let mut bgra = vec![0u8; (FB_W as usize) * (FB_H as usize) * 4];
        for y in 0..FB_H {
            for x in 0..FB_W {
                let [r, g, b, a] = img.pixel(x, y);
                let o = (y as usize * FB_W as usize + x as usize) * 4;
                bgra[o] = b;
                bgra[o + 1] = g;
                bgra[o + 2] = r;
                bgra[o + 3] = a;
            }
        }
        let sha = sc::hash::hex(&sc::hash::sha256(&std::fs::read(wp)?));
        verdict.set("wallpaper_png", J::s(wp));
        verdict.set("wallpaper_sha256", J::s(sha));
        if let Some(man) = f.val("wallpaper-manifest") {
            verdict.set("wallpaper_manifest", J::s(std::fs::read_to_string(man)?));
        }
        wall_bgra = Some(bgra);
    }

    // 3. Interactive kernel (polls the UART; paints input onto the strip)
    let kernel = demo::macho_interactive(0x30c040, strip_rows)?;
    verdict.set("kernel_source", J::s("demo-interactive"));
    let strip_top = FB_H.saturating_sub(strip_rows);

    // 4. ADT + payload (lowmem: fits 2G guest RAM on small hosts)
    let root = mkadt::build(&AdtOptions {
        qemu: true,
        product: None,
        phys_override: Some((mkadt::LOW_PHYS_BASE, mkadt::LOW_MEM_SIZE)),
        ..Default::default()
    })?;
    let adt_blob = adt::serialize(&root);
    ipsw::save(&outdir.join("qemu.adt"), &adt_blob)?;
    let (pl, info) = payload::build(&kernel, &adt_blob, "debug=0xc serial=3 cpus=1 keepsyms=1 rd=md0", None)?;
    let payload_path = outdir.join("payload.bin");
    ipsw::save(&payload_path, &pl)?;
    eprintln!("{}", info.summary());

    // 5. Boot the live session
    let gdb_port = sc::control::free_tcp_port()?;
    let sock = outdir.join("serial.sock");
    let opts = RunOptions {
        secs,
        cpu: f.val("cpu").unwrap_or("cortex-a76").to_string(),
        logo: logo_blob.clone(),
        serial_sock: Some(sock),
        ram_file: Some(outdir.join("ram.mmap")),
        qemu_datadir: f.val("qemu-datadir").map(|d| vec![d.to_string()]).unwrap_or_default(),
        ..Default::default()
    };
    let mut sess = sc::control::Session::boot(&payload_path, &outdir, &opts, gdb_port)?;
    let mut pass = true;

    // 6. Wait for the interactive kernel marker. If the socket connect raced
    //    the boot-time banner, Enter makes the guest reprint it.
    let marker = "SWEET-RUST interactive kernel OK";
    let mut marker_ok = sess.serial.wait_marker(marker, 8.0);
    if !marker_ok {
        let _ = sess.serial.send(b"\n");
        marker_ok = sess.serial.wait_marker(marker, 15.0);
    }
    verdict.set("serial_marker", J::b(marker_ok));

    // 7. Screenshot stage 1: the Apple logo from the IPSW
    let (p1, fb1) = sess.shot("fb-stage1-logo.png")?;
    eprintln!("[*] stage1 -> {}", p1.display());

    // 8. CONTROL ACTION: repaint the framebuffer with the real wallpaper
    if let Some(wall) = &wall_bgra {
        let fb_base = sc::FB_BASE;
        sess.poke(fb_base, wall)?;
        eprintln!("[*] wallpaper painted into guest framebuffer at {fb_base:#x}");
    }
    let (p2, fb2) = sess.shot("fb-stage2-wallpaper.png")?;
    eprintln!("[*] stage2 -> {}", p2.display());

    // 9. CONTROL ACTION: type into the guest UART
    sess.serial.send(send_text.as_bytes())?;
    verdict.set("sent_text", J::s(send_text.clone()));
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs_f64() < 4.0 {
        sess.serial.pump()?;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let (p3, fb3) = sess.shot("fb-stage3-input.png")?;
    eprintln!("[*] stage3 -> {}", p3.display());

    // 10. verdicts
    let stride = FB_STRIDE_US as usize;
    let lum1 = mean_luminance(&fb1, 0, FB_H / 3);
    let lum2 = mean_luminance(&fb2, 0, FB_H / 3);
    let logo_visible = lum1 < 90.0 && fb_mean_abs_diff(&fb1, &fb2) > 8.0;
    let wallpaper_visible = fb_mean_abs_diff(&fb1, &fb2) > 8.0 && lum2 > lum1;
    let input_echoed = sess.serial.text().contains(send_text.as_str());
    let input_visible = fb_diff_pixels(&fb2, &fb3, strip_top, FB_H) > 8;
    // raw BGRA corner bytes of the word 0xff30c040 (little endian)
    let corner_ok = fb1.len() >= 4 && fb1[0..4] == [0x40, 0xc0, 0x30, 0xff];
    verdict.set("kernel_alive", J::b(marker_ok || corner_ok || input_echoed));
    verdict.set("fb_corner_marker", J::b(corner_ok));
    verdict.set("logo_visible", J::b(logo_visible));
    verdict.set("wallpaper_visible", J::b(wallpaper_visible));
    verdict.set("input_echoed", J::b(input_echoed));
    verdict.set("input_visible", J::b(input_visible));
    verdict.set("serial_tail", J::s({
        let t = sess.serial.text();
        let cut = t.len().saturating_sub(400);
        t[cut..].to_string()
    }));
    pass = pass
        && (marker_ok || corner_ok)
        && wallpaper_visible
        && (wall_bgra.is_none() || input_visible || input_echoed);

    sess.kill();
    verdict.set("pass", J::b(pass));
    write_json(&outdir.join("emu-verdict.json"), &verdict)?;
    eprintln!("{}", verdict.dump());
    if !pass {
        bail!("EXIT3: interactive control verdict failed");
    }
    Ok(())
}

/// emu-peek: boot a payload, pause the VM, hex-dump guest RAM via gdbstub.
fn cmd_emu_peek(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("emu-peek"));
    if f.rest.len() < 2 {
        bail!("usage: emu-peek <PAYLOAD> <OUTDIR> [--addr HEX] [--len N] [--secs 6]");
    }
    let payload_path = PathBuf::from(f.rest[0]);
    let outdir = PathBuf::from(f.rest[1]);
    std::fs::create_dir_all(&outdir)?;
    let addr = f.num("addr", sc::FB_BASE);
    let len = f.num("len", 256) as usize;
    let secs: f64 = f.val("secs").and_then(|v| v.parse().ok()).unwrap_or(6.0);
    let gdb_port = sc::control::free_tcp_port()?;
    let sock = outdir.join("serial.sock");
    let opts = RunOptions {
        secs,
        cpu: f.val("cpu").unwrap_or("cortex-a76").to_string(),
        serial_sock: Some(sock),
        ..Default::default()
    };
    let mut sess = sc::control::Session::boot(&payload_path, &outdir, &opts, gdb_port)?;
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    let mem = sess.peek(addr, len)?;
    sess.kill();
    for (i, chunk) in mem.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| if (32..127).contains(&b) { b as char } else { '.' })
            .collect();
        println!("{:08x}: {:48}  {}", addr + (i * 16) as u64, hex.join(" "), ascii);
    }
    Ok(())
}

/// Mean 8-bit luminance of an FB dump row range.
fn mean_luminance(fb: &[u8], row_from: u32, row_to: u32) -> f64 {
    let stride = FB_STRIDE_US as usize;
    let from = (row_from as usize).min(crate::FB_H as usize);
    let to = (row_to as usize).min(crate::FB_H as usize);
    let mut acc: u64 = 0;
    let mut cnt: u64 = 0;
    for y in from..to {
        let base = y * stride;
        let mut x = 0usize;
        while x + 3 < crate::FB_W as usize {
            let o = base + x * 4;
            if o + 3 >= fb.len() {
                break;
            }
            // BGRA: b,g,r
            acc += (fb[o] as u64 * 7 + fb[o + 1] as u64 * 42 + fb[o + 2] as u64 * 21) / 70;
            cnt += 1;
            x += 7;
        }
    }
    if cnt == 0 { 0.0 } else { acc as f64 / cnt as f64 }
}

/// A local path is a raw dmg (not an IPSW zip) when its tail has no zip EOCD.
fn local_is_raw_dmg(path: &str) -> Result<bool> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() || meta.len() < 64 {
        return Ok(false);
    }
    let tail = ipsw::read_at(path, meta.len() - 64, 64)?;
    Ok(!tail.windows(4).any(|w| w == [0x50, 0x4b, 0x05, 0x06]))
}

/// hello-extract: pull + render the real welcome-screen vector data.
fn cmd_hello_extract(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("hello-extract"));
    if f.rest.is_empty() {
        bail!("usage: hello-extract <IPSW|URL> <OUTDIR> [--lang en_GB]");
    }
    let source = f.val("ipsw").unwrap_or_else(|| f.rest[0].clone());
    let outdir = PathBuf::from(f.rest.get(1).map(|s| s.to_string()).unwrap_or_else(|| ".".to_string()));
    std::fs::create_dir_all(&outdir)?;
    let lang = f.val("lang").unwrap_or("en_GB").to_string();
    let (json, src_path, sha) = hello_json_from_ipsw(&source, &lang)?;
    let json_path = outdir.join(format!("CursiveHello_{lang}.json"));
    ipsw::save(&json_path, &json)?;
    let fb = sc::hello::welcome_fb_from_json(&json)?;
    let png_path = outdir.join(format!("hello_{lang}.png"));
    ipsw::save(&png_path, &sc::png::encode_fb(FB_W, FB_H, &fb)?)?;
    let mut prov = J::obj();
    prov.set("source", J::s(src_path));
    prov.set("sha256", J::s(sha));
    prov.set("bytes", J::n(json.len() as f64));
    prov.set("lang", J::s(lang));
    prov.set("note", J::s("vector data (cubic bezier strokes) rendered verbatim; no hand-drawn stand-ins"));
    write_json(&outdir.join("provenance.json"), &prov)?;
    eprintln!("hello-extract: {} bytes, sha256 provenance written", json.len());
    Ok(())
}

fn logo_image(source: &str) -> Result<(sc::png::Image, String)> {
    let idx = ipsw::index(source)?;
    let logo = find_logo(&idx)?;
    let raw = ipsw::extract(source, logo)?;
    let (payload, info) = im4p::unwrap(&raw)?;
    // all_flash assets are iBootIm containers (sszl/grey); the IPSW may also
    // carry plain PNGs for older targets.
    let img = if sc::ibootim::looks_ibootim(&payload) {
        let ib = sc::ibootim::decode(&payload)?;
        eprintln!("iBootIm asset: {} {}x{}", ib.format, ib.width, ib.height);
        ib.to_image()
    } else {
        sc::png::decode(&payload).map_err(|e| {
            sc::Error::Msg(format!(
                "applelogo decode failed ({:?}, {} bytes): {e}",
                info.kind,
                payload.len()
            ))
        })?
    };
    Ok((img, logo.name.clone()))
}

fn write_json(path: &Path, v: &J) -> Result<()> {
    std::fs::write(path, format!("{}\n", v.dump()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

fn cmd_ipsw_info(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: ipsw-info <IPSW|URL>");
    }
    let f = Flags::parse(args, &value_flags("ipsw-info"));
    let src = f.rest[0];
    let idx = ipsw::index(src)?;
    let mut total = 0u64;
    for e in &idx.entries {
        println!("{:>14}  {:>14}  {:04x}  {}", e.size, e.comp_size, e.method, e.name);
        total += e.size;
    }
    eprintln!("{} members, {total} bytes uncompressed", idx.entries.len());
    Ok(())
}

fn cmd_ipsw_extract(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("ipsw-extract"));
    let src = f.rest.first().ok_or_else(|| sc::Error::Msg("usage: ipsw-extract <IPSW|URL> --member NAME --out DIR".into()))?;
    // repeated --member flags don't fit a map: scan the raw args
    let mut members: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--member" && i + 1 < args.len() {
            members.push(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }
    if members.is_empty() || !f.has("out") {
        bail!("usage: ipsw-extract <IPSW|URL> --member NAME [--member ...] --out DIR");
    }
    let idx = ipsw::index(src)?;
    let out = PathBuf::from(f.val("out").unwrap());
    let mut n = 0;
    for m in &members {
        for e in idx.entries.iter().filter(|e| e.name == *m || e.name.contains(m.as_str())) {
            let mut data = ipsw::extract(src, e)?;
            // dmg/img3 members are IM4P-wrapped (LZFSE): unwrap so the raw
            // APFS / payload bytes land on disk ready for Dmg::open_auto.
            if sc::im4p::looks_im4p(&data) {
                match sc::im4p::unwrap(&data) {
                    Ok((raw, info)) => {
                        eprintln!("  im4p: {} {} -> {} bytes", info.kind, info.desc, raw.len());
                        data = raw;
                    }
                    Err(e) => eprintln!("  im4p unwrap failed ({e}); saving wrapped"),
                }
            }
            let dest = out.join(e.name.replace('/', "_"));
            ipsw::save(&dest, &data)?;
            println!("{} -> {} ({} bytes)", e.name, dest.display(), data.len());
            n += 1;
        }
    }
    if n == 0 {
        bail!("no members matched {members:?}");
    }
    Ok(())
}

fn cmd_kernel_unpack(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: kernel-unpack <IN.im4p> <OUT.macho>");
    }
    let raw = std::fs::read(&args[0])?;
    let (payload, info) = im4p::unwrap(&raw)?;
    ipsw::save(Path::new(&args[1]), &payload)?;
    eprintln!(
        "{}: {:?} {} -> {} bytes (encrypted={})",
        args[0], info.kind, info.desc, payload.len(), info.encrypted
    );
    let m = sc::macho::parse_best(&payload)?.0;
    eprintln!("macho: cputype={:#x} segs={} entry={:#x}", m.cputype, m.segments.len(), m.entry);
    Ok(())
}

fn cmd_make_adt(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("make-adt"));
    if f.rest.is_empty() {
        bail!("usage: make-adt <OUT.adt> [--qemu] [--highmem] [--product FILE] [--seed HEX] [--dump]");
    }
    let product = f.val("product").map(|p| -> Result<Vec<(String, Vec<u8>)>> {
        let buf = load_adtish(Path::new(p))?;
        mkadt::d22ap_product(&buf)
    }).transpose()?;
    let seed = f.val("seed").map(|s| {
        let s = s.trim_start_matches("0x");
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16))
            .collect::<std::result::Result<Vec<u8>, _>>()
            .expect("bad --seed hex")
    });
    let phys_override = match (f.val("phys"), f.val("mem")) {
        (Some(p), Some(m)) => Some((
            u64::from_str_radix(p.trim_start_matches("0x"), 16).expect("bad --phys"),
            u64::from_str_radix(m.trim_start_matches("0x"), 16).expect("bad --mem"),
        )),
        _ => None,
    };
    let opts = AdtOptions {
        qemu: f.has("qemu"),
        highmem: f.has("highmem"),
        product: product.as_deref(),
        seed,
        phys_override,
    };
    let root = mkadt::build(&opts)?;
    let blob = adt::serialize(&root);
    if f.has("dump") {
        print!("{}", adt::dump(&adt::parse(&blob)?.0, 0, false));
    }
    ipsw::save(Path::new(f.rest[0]), &blob)?;
    eprintln!("{}: {} bytes", f.rest[0], blob.len());
    Ok(())
}

fn cmd_make_payload(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("make-payload"));
    if f.rest.len() < 3 {
        bail!("usage: make-payload <KERNEL> <ADT> <OUT> [--cmdline \"...\"] [--ramdisk IMAGE]");
    }
    let kernel = std::fs::read(f.rest[0])?;
    let adt = load_adtish(Path::new(f.rest[1]))?;
    let ramdisk = match f.val("ramdisk") {
        Some(p) => Some(std::fs::read(p)?),
        None => None,
    };
    let cmdline = f.val("cmdline").unwrap_or("debug=0xc serial=3 cpus=1 keepsyms=1");
    let (blob, info) = payload::build(&kernel, &adt, cmdline, ramdisk.as_deref())?;
    ipsw::save(Path::new(f.rest[2]), &blob)?;
    eprintln!("{}", info.summary());
    Ok(())
}

fn cmd_make_bootimg(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("make-bootimg"));
    let kernel = match f.val("kernel") {
        Some(p) => std::fs::read(p)?,
        None => bail!("--kernel required"),
    };
    let ramdisk = match f.val("ramdisk") {
        Some(p) if !p.is_empty() => std::fs::read(p)?,
        _ => cpio::empty_cpio()?, // ABL dislikes a zero-size ramdisk
    };
    let dtb = f.val("dtb").map(|p| std::fs::read(p).unwrap()).filter(|d| !d.is_empty());
    let out = f.val("o").unwrap_or("shim-boot.img");
    let cmdline = f.val("cmdline").unwrap_or("console=null");
    let opts = bootimg::BootImageOptions {
        kernel: &kernel,
        ramdisk: &ramdisk,
        dtb: dtb.as_deref(),
        cmdline,
        ..Default::default()
    };
    let img = bootimg::build_v2(&opts)?;
    ipsw::save(Path::new(out), &img)?;
    eprintln!("{out}: {} bytes (kernel {}, ramdisk {})", img.len(), kernel.len(), ramdisk.len());
    Ok(())
}

fn cmd_logo_extract(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: logo-extract <IPSW|URL> <OUT.png>");
    }
    let (img, name) = logo_image(&args[0])?;
    let mut rgb = Vec::with_capacity(img.width as usize * img.height as usize * 3);
    for y in 0..img.height {
        for x in 0..img.width {
            let p = img.pixel(x, y);
            rgb.extend_from_slice(&[p[0], p[1], p[2]]);
        }
    }
    let png = sc::png::encode_rgb(img.width, img.height, &rgb)?;
    ipsw::save(Path::new(&args[1]), &png)?;
    eprintln!("{}: {}x{} -> {}", name, img.width, img.height, args[1]);
    Ok(())
}

fn cmd_fb_render(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("fb-render"));
    if f.rest.len() < 2 {
        bail!("usage: fb-render <IPSW|URL> <OUT.png> [--logo-scale 33]");
    }
    let (img, name) = logo_image(f.rest[0])?;
    eprintln!("logo: {name} {}x{}", img.width, img.height);
    let fb = fblogo::compose(&img, f.num("logo-scale", 33) as u32);
    let png = sc::png::encode_fb(FB_W, FB_H, &fb)?;
    ipsw::save(Path::new(f.rest[1]), &png)?;
    eprintln!("{}: {}x{} BGRA canvas -> PNG", f.rest[1], FB_W, FB_H);
    Ok(())
}

fn cmd_inject_logo(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: inject-logo <IPSW|URL> <OUT.logo.bin> [--logo-scale 33]");
    }
    let f = Flags::parse(args, &value_flags("inject-logo"));
    let (img, name) = logo_image(f.rest[0])?;
    let fb = fblogo::compose(&img, f.num("logo-scale", 33) as u32);
    let blob = fblogo::loader_blob(&fb)?;
    ipsw::save(Path::new(f.rest[1]), &blob)?;
    eprintln!("{name} -> {} ({} bytes, for -device loader,addr=0x9c000000)", f.rest[1], blob.len());
    Ok(())
}

fn cmd_demo_kernel(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("demo-kernel"));
    if f.rest.is_empty() {
        bail!("usage: demo-kernel <OUT.macho> [--color 0x30c040] [--strip 400]");
    }
    let color = f.num("color", 0x30c040) as u32;
    let strip = f.num("strip", 400) as u32;
    let km = if f.has("interactive") {
        demo::macho_interactive(color, strip)?
    } else {
        demo::macho(color, strip)?
    };
    ipsw::save(Path::new(f.rest[0]), &km)?;
    eprintln!("{}: {} bytes (color {color:#x}, strip {strip} rows)", f.rest[0], km.len());
    Ok(())
}

fn cmd_boot_qemu(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("boot-qemu"));
    if f.has("check-env") {
        let ok = qemu::check_env(&f.val("cpu").unwrap_or("cortex-a76"));
        if !ok {
            bail!("QEMU environment check failed");
        }
        return Ok(());
    }
    if f.rest.len() < 2 {
        bail!("usage: boot-qemu <PAYLOAD> <OUTDIR> [options]");
    }
    let mut opts = RunOptions {
        secs: f.val("secs").and_then(|v| v.parse().ok()).unwrap_or(20.0),
        gdb: f.has("gdb"),
        cpu: f.val("cpu").unwrap_or("cortex-a76").to_string(),
        live_serial: f.has("live-serial"),
        qemu_datadir: f.val("qemu-datadir").map(|d| vec![d.to_string()]).unwrap_or_default(),
        ..Default::default()
    };
    if let Some(hmps) = f.val("hmp") {
        opts.hmp = hmps.split(';').map(|s| s.to_string()).collect();
    }
    if let Some(logo_path) = f.val("logo") {
        opts.logo = Some(std::fs::read(logo_path)?);
    }
    if let Some(ipsw) = f.val("logo-ipsw") {
        let (img, name) = logo_image(ipsw)?;
        eprintln!("logo from IPSW: {name}");
        opts.logo = Some(fblogo::loader_blob(&fblogo::compose(&img, 33))?);
    }
    let _payload_check = std::fs::metadata(f.rest[0])?;
    match qemu::run(Path::new(f.rest[0]), Path::new(f.rest[1]), &opts, None) {
        Ok(o) => {
            eprintln!("status: {}", o.result.dump());
            Ok(())
        }
        Err(e) if e.to_string().starts_with("EXIT") => Err(e),
        Err(e) => Err(e),
    }
}

/// Debug/diagnostic: lay a payload out at its physical addresses (the RAM
/// image QEMU loads) without running QEMU.
fn cmd_place_image(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: place-image <PAYLOAD> <OUT.ram.bin>");
    }
    let payload = std::fs::read(&args[0])?;
    let placed = payload::build_image(&payload)?;
    ipsw::save(Path::new(&args[1]), &placed.image)?;
    eprintln!(
        "phys_base={:#x} boot_args={:#x} entry={:#x} image={} bytes",
        placed.phys_base,
        placed.boot_args_pa,
        placed.entry_pa,
        placed.image.len()
    );
    Ok(())
}

/// The full pipeline: IPSW -> Apple logo (+ kernel) -> payload -> QEMU.
fn cmd_ipsw_boot(args: &[String]) -> Result<()> {
    let f = Flags::parse(args, &value_flags("ipsw-boot"));
    if f.rest.is_empty() {
        bail!("usage: ipsw-boot --ipsw IPSW|URL <OUTDIR> [--kernel K | --demo] [--ramdisk R] [--highmem] [--cmdline ...] [--secs 120]");
    }
    let ipsw_src = f.val("ipsw").ok_or_else(|| sc::Error::Msg("--ipsw required".into()))?;
    let outdir = PathBuf::from(f.rest[0]);
    std::fs::create_dir_all(&outdir)?;
    let secs: f64 = f.val("secs").and_then(|v| v.parse().ok()).unwrap_or(120.0);
    let cmdline = f
        .val("cmdline")
        .unwrap_or("debug=0xc serial=3 cpus=1 keepsyms=1 rd=md0")
        .to_string();

    let mut verdict = J::obj();
    verdict.set("ipsw", J::s(ipsw_src));

    // 1. Apple logo asset from the IPSW
    let logo_scale = f.num("logo-scale", 33) as u32;
    let mut logo_blob = None;
    if !f.has("no-logo") {
        match logo_image(ipsw_src) {
            Ok((img, name)) => {
                eprintln!("logo from IPSW: {name} ({}x{})", img.width, img.height);
                verdict.set("logo_member", J::s(name));
                let png = sc::png::encode_fb(FB_W, FB_H, &fblogo::compose(&img, logo_scale))?;
                ipsw::save(&outdir.join("logo-fb.png"), &png)?;
                logo_blob = Some(fblogo::loader_blob(&fblogo::compose(&img, logo_scale))?);
                verdict.set("logo_scale", J::n(logo_scale as f64));
            }
            Err(e) => {
                eprintln!("[!] logo extraction failed: {e} (continuing without logo)");
                verdict.set("logo_member", J::Null);
            }
        }
    }

    // 2. kernel: user file, or the demo kernel
    let hello = f.has("hello");
    let kernel: Vec<u8> = if let Some(k) = f.val("kernel") {
        verdict.set("kernel_source", J::s(k));
        std::fs::read(k)?
    } else if hello {
        let lang = f.val("hello-lang").unwrap_or("en_GB").to_string();
        let (json, src_path, sha) = hello_json_from_ipsw(ipsw_src, &lang)?;
        verdict.set("kernel_source", J::s("demo-hello"));
        verdict.set("hello_lang", J::s(lang));
        verdict.set("hello_source", J::s(src_path));
        verdict.set("hello_sha256", J::s(sha));
        verdict.set("hello_json_bytes", J::n(json.len() as f64));
        let fb = sc::hello::welcome_fb_from_json(&json)?;
        sc::ipsw::save(&outdir.join("hello-fb.png"), &sc::png::encode_fb(FB_W, FB_H, &fb)?)?;
        sc::ipsw::save(&outdir.join("CursiveHello.json"), &json)?;
        eprintln!("hello: real IPSW vector data ({} bytes) rendered", json.len());
        demo::macho_hello(&fb)?
    } else if f.has("demo") {
        verdict.set("kernel_source", J::s("demo"));
        demo::macho(0x30c040, 400)?
    } else {
        // last resort: try the IPSW's own kernelcache (stock: boots only on
        // Apple hardware, but the payload pipeline is still exercised)
        let idx = ipsw::index(ipsw_src)?;
        let kc = idx
            .entries
            .iter()
            .find(|e| e.name.contains("kernelcache"))
            .ok_or_else(|| sc::Error::Msg("no --kernel/--demo and no kernelcache in IPSW".into()))?;
        let raw = ipsw::extract(ipsw_src, kc)?;
        let (k, info) = im4p::unwrap(&raw)?;
        eprintln!("kernelcache: {:?} {} -> {} bytes", info.kind, kc.name, k.len());
        verdict.set("kernel_source", J::s(kc.name.clone()));
        k
    };

    // 3. ADT (+ optional real /product from the IPSW DeviceTree)
    let product = f.val("product").map(|p| -> Result<Vec<(String, Vec<u8>)>> {
        let buf = load_adtish(Path::new(p))?;
        mkadt::d22ap_product(&buf)
    }).transpose()?;
    let phys_override = if f.has("lowmem") {
        Some((mkadt::LOW_PHYS_BASE, mkadt::LOW_MEM_SIZE))
    } else if f.has("highmem") {
        // multi-GB md0 (real rootfs) needs the upper DDR bank: ramdisk PA =
        // kernel_end must stay inside [phys_base, phys_base+mem_size).
        Some((mkadt::HIGH_PHYS_BASE, mkadt::HIGH_MEM_SIZE))
    } else {
        None
    };
    let root = mkadt::build(&AdtOptions {
        qemu: true,
        product: product.as_deref(),
        phys_override,
        ..Default::default()
    })?;
    let adt_blob = adt::serialize(&root);
    ipsw::save(&outdir.join("qemu.adt"), &adt_blob)?;

    // 4. payload (+ ramdisk)
    let ramdisk = f.val("ramdisk").map(|p| std::fs::read(p)).transpose()?;
    let (pl, info) = payload::build(&kernel, &adt_blob, &cmdline, ramdisk.as_deref())?;
    let payload_path = outdir.join("payload.bin");
    ipsw::save(&payload_path, &pl)?;
    eprintln!("{}", info.summary());
    verdict.set("payload_size", J::n(pl.len() as f64));
    verdict.set("fixups", J::n(info.fixups as f64));
    // MEMORY: the loader reads payload.bin from disk (-device loader,file=…),
    // so the in-RAM copies are dead weight from here on. On a 16GB CI runner
    // a 4.2GB root held twice (ramdisk + payload) plus QEMU's resident
    // shared-memory backend OOMs the agent (job dies as "The operation was
    // canceled" ~45s into the boot — the 2026-10-23 M3 run cliff).
    drop(pl);
    drop(ramdisk);
    drop(kernel);
    drop(adt_blob);

    // 5. QEMU boot with the logo loader device
    let mut opts = RunOptions {
        secs,
        cpu: f.val("cpu").unwrap_or("cortex-a76").to_string(),
        live_serial: f.has("live-serial"),
        logo: if f.has("no-logo") { None } else { logo_blob.clone() },
        qemu_datadir: f.val("qemu-datadir").map(|d| vec![d.to_string()]).unwrap_or_default(),
        ..Default::default()
    };
    let boot = match qemu::run(&payload_path, &outdir, &opts, None) {
        Ok(o) => Some(o),
        Err(e) if e.to_string().starts_with("EXIT1") => {
            // qemu exited early — still write a verdict
            None
        }
        Err(e) if e.to_string().starts_with("EXIT") => {
            write_json(&outdir.join("boot-verdict.json"), &{
                let mut v = J::obj();
                v.set("pass", J::b(false));
                v.set("reason", J::s(e.to_string()));
                v
                })?;
            return Err(e);
        }
        Err(e) => return Err(e),
    };

    // 6. verdict
    let serial = std::fs::read_to_string(outdir.join("serial.txt")).unwrap_or_default();
    let fb_png = sc::png::decode(&std::fs::read(outdir.join("fb.png")).unwrap_or_default()).ok();
    let hello_marker = serial.contains(sc::hello::SERIAL_MARKER);
    let demo_marker =
        serial.contains("SWEET-RUST demo kernel OK") || hello_marker;
    let panic = serial.contains("panic(cpu ") || serial.contains("SWEET EARLY PANIC:");
    let bsd_root = serial.contains("BSD root: md0");
    let pid1 = serial.contains("init running as PID");
    // deterministic demo-kernel marker: it paints fb pixel (0,0) with its color
    let kernel_ran = fb_png
        .as_ref()
        .map(|img| img.pixel(0, 0) == [0x30, 0xc0, 0x40, 255])
        .unwrap_or(false);
    let mut pass = demo_marker || kernel_ran || bsd_root || pid1;
    if f.val("kernel").is_some() {
        // REAL SWEET kernel boot: the console goes to the framebuffer
        // (serial stays empty), so the meaningful gate is "no panic + the
        // kernel produced visible console output / reached BSD root". The
        // workflow layer re-checks the decoded fb text (screen-decode) for
        // the actual milestone markers (BSD root: md0, launchd, ...).
        let fb_active = fb_png.as_ref()
            .map(|img| {
                let mut lit = 0u64;
                for y in (0..img.height.min(FB_H)).step_by(4) {
                    for x in (0..img.width.min(FB_W)).step_by(4) {
                        let [r, g, b, _] = img.pixel(x, y);
                        if r + g + b > 60 {
                            lit += 1;
                        }
                    }
                }
                let samples = ((img.height.min(FB_H) / 4) * (img.width.min(FB_W) / 4)) as f64;
                lit as f64 / samples.max(1.0)
            })
            .unwrap_or(0.0);
        verdict.set("fb_console_fraction", J::n(fb_active));
        pass = !panic && (bsd_root || pid1 || fb_active > 0.0005);
    }
    if let Some(img) = &fb_png {
        // reconstruct the BGRA canvas from the decoded PNG to analyze it
        let mut fb = vec![0u8; (FB_W * 4 * FB_H) as usize];
        for y in 0..img.height.min(FB_H) {
            for x in 0..img.width.min(FB_W) {
                let [r, g, b, _] = img.pixel(x, y);
                let o = y as usize * FB_STRIDE_US as usize + x as usize * 4;
                fb[o] = b;
                fb[o + 1] = g;
                fb[o + 2] = r;
                fb[o + 3] = 255;
            }
        }
        verdict.set("fb_nonblack", J::n(fblogo::nonblack_fraction(&fb)));
        verdict.set("fb_bright_center", J::n(fblogo::bright_fraction(&fb, 120)));
        if hello_marker {
            verdict.set("hello_visible", J::b(true));
            verdict.set("logo_visible", J::b(false));
            pass = true;
        } else if f.has("hello") && fblogo::is_hello_screen(&fb) {
            // serial capture is unreliable under TCG (0 bytes observed);
            // the hello screen is unambiguous in the FB: solid-white text
            // (>=250) in the center box over an almost-black background —
            // the dithered logo never satisfies both.
            verdict.set("hello_visible", J::b(true));
            verdict.set("hello_detection", J::s("framebuffer"));
            verdict.set("logo_visible", J::b(false));
            pass = true;
        } else if logo_blob.is_some() && !f.has("no-logo") {
            let bright = fblogo::bright_fraction(&fb, 120);
            verdict.set("logo_visible", J::b(bright > 0.005 && !panic));
            pass = pass || bright > 0.005;
        }
    }
    verdict.set("serial_bytes", J::n(serial.len() as f64));
    verdict.set("demo_marker", J::b(demo_marker || kernel_ran));
    verdict.set("bsd_root_md0", J::b(bsd_root));
    verdict.set("pid1", J::b(pid1));
    verdict.set("panic", J::b(panic));
    verdict.set("pass", J::b(pass && !panic));
    write_json(&outdir.join("boot-verdict.json"), &verdict)?;
    eprintln!("{}", verdict.dump());
    if !pass || panic {
        bail!("EXIT1: boot verdict failed");
    }
    Ok(())
}

const FB_STRIDE_US: u32 = FB_W * 4;
