//! sweet-rust — the ios-sweet boot pipeline, in Rust.
//!
//! One binary, std-only + vendored native C (Apple LZFSE, miniz):
//!   ipsw-info | ipsw-extract | kernel-unpack | make-adt | make-payload
//!   make-bootimg | logo-extract | fb-render | inject-logo | demo-kernel
//!   boot-qemu | ipsw-boot

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
        "kernel-unpack" => cmd_kernel_unpack(rest),
        "make-adt" => cmd_make_adt(rest),
        "make-payload" => cmd_make_payload(rest),
        "make-bootimg" => cmd_make_bootimg(rest),
        "logo-extract" => cmd_logo_extract(rest),
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
        "kernel-unpack" => vec![],
        "make-adt" => vec!["product", "seed", "phys", "mem"],
        "make-payload" => vec!["cmdline", "ramdisk"],
        "make-bootimg" => vec!["kernel", "ramdisk", "dtb", "cmdline", "o"],
        "logo-extract" | "inject-logo" => vec![],
        "fb-render" => vec!["w", "h", "logo-scale"],
        "demo-kernel" => vec!["color", "strip"],
        "boot-qemu" => vec!["secs", "cpu", "logo", "logo-ipsw", "qemu-datadir", "hmp"],
        "ipsw-boot" => vec![
            "ipsw", "kernel", "ramdisk", "cmdline", "secs", "product",
            "logo-scale", "cpu", "w", "h", "qemu-datadir",
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
            let data = ipsw::extract(src, e)?;
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
    let m = sc::macho::parse(&payload)?;
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
    let km = demo::macho(color, strip)?;
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
        bail!("usage: ipsw-boot --ipsw IPSW|URL <OUTDIR> [--kernel K | --demo] [--ramdisk R] [--cmdline ...] [--secs 120]");
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
    let kernel: Vec<u8> = if let Some(k) = f.val("kernel") {
        verdict.set("kernel_source", J::s(k));
        std::fs::read(k)?
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
    let demo_marker = serial.contains("SWEET-RUST demo kernel OK");
    let panic = serial.contains("panic(cpu ") || serial.contains("SWEET EARLY PANIC:");
    let bsd_root = serial.contains("BSD root: md0");
    let pid1 = serial.contains("init running as PID");
    // deterministic demo-kernel marker: it paints fb pixel (0,0) with its color
    let kernel_ran = fb_png
        .as_ref()
        .map(|img| img.pixel(0, 0) == [0x30, 0xc0, 0x40, 255])
        .unwrap_or(false);
    let mut pass = demo_marker || kernel_ran || bsd_root || pid1;
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
        if let Some(rd) = info.ramdisk {
            let _ = rd;
        }
        if logo_blob.is_some() && !f.has("no-logo") {
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
