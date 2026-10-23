//! Test suite — ports of `scripts/test_payload_pipeline.py` (16/16 checks),
//! `scripts/test_qmp_branches.py` (unix + TCP QMP against a fake server) and
//! new round-trips through the vendored C libraries.

use crate::adt;
use crate::bootimg;
use crate::cpio;
use crate::demo;
use crate::im4p;
use crate::ipsw;
use crate::mkadt::{self, AdtOptions};
use crate::payload;
use crate::qemu;
use crate::qemu::jsonlite::J;

fn adt_qemu() -> Vec<u8> {
    let root = mkadt::build(&AdtOptions { qemu: true, ..Default::default() }).unwrap();
    adt::serialize(&root)
}

/// A synthetic "kernel" with two segments and chained-fixup pointers, in the
/// format the Python test used (port of test_payload_pipeline.py's fake).
fn synthetic_kernel() -> Vec<u8> {
    // __TEXT at 0xfffffe000c000000 (a plausible xnu VA), __DATA follows.
    const TEXT_VA: u64 = 0xfffffe000c000000;
    const TEXT_SIZE: u64 = 0x4000;
    const DATA_VA: u64 = 0xfffffe000c004000;
    const DATA_SIZE: u64 = 0x4000;
    let ncmds = 4u32; // SEG64, SEG64, UNIXTHREAD, DYLD_CHAINED_FIXUPS
    let mut cmds: Vec<u8> = Vec::new();

    let seg = |name: &str, vmaddr: u64, vmsize: u64, fileoff: u64, filesize: u64| -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(&0x19u32.to_le_bytes());
        c.extend_from_slice(&72u32.to_le_bytes());
        let mut nm = [0u8; 16];
        nm[..name.len()].copy_from_slice(name.as_bytes());
        c.extend_from_slice(&nm);
        c.extend_from_slice(&vmaddr.to_le_bytes());
        c.extend_from_slice(&vmsize.to_le_bytes());
        c.extend_from_slice(&fileoff.to_le_bytes());
        c.extend_from_slice(&filesize.to_le_bytes());
        c.extend_from_slice(&7u32.to_le_bytes()); // maxprot rwx
        c.extend_from_slice(&5u32.to_le_bytes()); // initprot rx
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c
    };
    // __TEXT: header + cmds + code region
    let text_filesize = 0x1000u64; // file part only
    cmds.extend_from_slice(&seg("__TEXT", TEXT_VA, TEXT_SIZE, 0, text_filesize));
    cmds.extend_from_slice(&seg("__DATA", DATA_VA, DATA_SIZE, text_filesize, 0x200));

    // LC_UNIXTHREAD with pc = TEXT_VA + 0x1000
    let entry = TEXT_VA + 0x1000;
    cmds.extend_from_slice(&5u32.to_le_bytes());
    cmds.extend_from_slice(&((16 + 4 * 68) as u32).to_le_bytes());
    cmds.extend_from_slice(&6u32.to_le_bytes());
    cmds.extend_from_slice(&68u32.to_le_bytes());
    let mut state = [0u64; 33];
    state[32] = entry;
    for v in state {
        cmds.extend_from_slice(&v.to_le_bytes());
    }
    cmds.extend_from_slice(&0u32.to_le_bytes());
    cmds.extend_from_slice(&0u32.to_le_bytes());

    // LC_DYLD_CHAINED_FIXUPS with one page-start table for __DATA:
    // format 6 (DYLD_CHAINED_PTR_64_OFFSET), one pointer rebase.
    // LC_DYLD_CHAINED_FIXUPS: the fixup header follows the load command
    // (dataoff = file offset of the bytes we append right after this cmd)
    let fix_data_off = (32 + cmds.len() + 16) as u32; // after cmd+size+dataoff+datasize
    let mut fh: Vec<u8> = Vec::new();
    fh.extend_from_slice(&0u32.to_le_bytes()); // fixups_version
    fh.extend_from_slice(&28u32.to_le_bytes()); // starts_offset (relative to data_off; starts right after the 7 u32 header)
    fh.extend_from_slice(&0u32.to_le_bytes()); // imports_offset
    fh.extend_from_slice(&0u32.to_le_bytes()); // symbols_offset
    fh.extend_from_slice(&0u32.to_le_bytes()); // imports_count
    fh.extend_from_slice(&0u32.to_le_bytes()); // symbols_count
    fh.extend_from_slice(&0u32.to_le_bytes()); // other
    // starts section at data_off + 36:
    fh.extend_from_slice(&2u32.to_le_bytes()); // seg count
    let seg0_info = 0u32; // __TEXT: none (0)
    let seg1_info = 12u32; // __DATA info at starts+12 (right after seg_count+seg0)
    fh.extend_from_slice(&seg0_info.to_le_bytes());
    fh.extend_from_slice(&seg1_info.to_le_bytes());
    // __DATA seg info (<IHHQIH): size, page_size 0x1000, fmt 6, offset 0 (u64), max_valid, page_count 1
    let mut si: Vec<u8> = Vec::new();
    si.extend_from_slice(&26u32.to_le_bytes()); // size of this struct incl starts
    si.extend_from_slice(&0x1000u16.to_le_bytes()); // page_size
    si.extend_from_slice(&6u16.to_le_bytes()); // fmt 6
    si.extend_from_slice(&0u64.to_le_bytes()); // seg_offset (u64!)
    si.extend_from_slice(&0u32.to_le_bytes()); // max_valid
    si.extend_from_slice(&1u32.to_le_bytes()); // page_count
    si.extend_from_slice(&0u16.to_le_bytes()); // page 0 starts at 0
    fh.extend_from_slice(&si);
    cmds.extend_from_slice(&0x8000_0034u32.to_le_bytes());
    cmds.extend_from_slice(&((8 + fh.len()) as u32).to_le_bytes());
    cmds.extend_from_slice(&fix_data_off.to_le_bytes());
    cmds.extend_from_slice(&(fh.len() as u32).to_le_bytes());
    cmds.extend_from_slice(&fh);

    // Assemble: header (32) + cmds, then __TEXT file bytes (zeroes + a
    // chained pointer at DATA_VA - DATA fileoff... place the pointer inside
    // the __DATA file bytes at seg-relative page 0 offset 0.
    let total_cmds = 32 + cmds.len();
    assert!(total_cmds <= text_filesize as usize);
    let mut img = vec![0u8; (text_filesize + 0x200) as usize];
    let mut header: Vec<u8> = Vec::new();
    header.extend_from_slice(&0xfeedfacfu32.to_le_bytes());
    header.extend_from_slice(&0x0100_000cu32.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&2u32.to_le_bytes());
    header.extend_from_slice(&ncmds.to_le_bytes());
    header.extend_from_slice(&(cmds.len() as u32).to_le_bytes());
    header.extend_from_slice(&1u32.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    img[..32].copy_from_slice(&header);
    img[32..32 + cmds.len()].copy_from_slice(&cmds);
    // The fixup pointer lives at file offset text_filesize (DATA page 0):
    // fmt 6 = offset format: target = raw36 + image_base.
    let target_va = TEXT_VA + 0x2000;
    let raw = (target_va - TEXT_VA) & ((1 << 36) - 1); // offset from image base
    img[text_filesize as usize..text_filesize as usize + 8]
        .copy_from_slice(&raw.to_le_bytes());
    img
}

#[test]
fn adt_roundtrip_and_qemu_values() {
    let blob = adt_qemu();
    let (root, _) = adt::parse(&blob).unwrap();
    assert_eq!(root.name(), "device-tree");
    let chosen = root.child("chosen").unwrap();
    let dram = <[u8; 8]>::try_from(chosen.prop("dram-base").unwrap()).unwrap();
    assert_eq!(u64::from_le_bytes(dram), mkadt::PHYS_BASE);
    // QEMU-specific values
    let gic = root
        .child("arm-io")
        .unwrap()
        .child("gic")
        .unwrap();
    let vti = <[u8; 4]>::try_from(gic.prop("virtual-timer-intid").unwrap()).unwrap();
    assert_eq!(u32::from_le_bytes(vti), mkadt::QEMU_VTIMER_INTID);
    // panic log carved off the top: /pram reg
    let pram = root.child("pram").unwrap();
    let reg = pram.prop("reg").unwrap();
    let panic_base = u64::from_le_bytes(reg[..8].try_into().unwrap());
    assert_eq!(panic_base, mkadt::PHYS_BASE + mkadt::MEM_SIZE - mkadt::PANIC_LOG_SIZE);
    // serialize(parse(x)) is byte-stable
    let again = adt::serialize(&root);
    assert_eq!(blob, again);
    // highmem variant
    let hi = mkadt::build(&AdtOptions { qemu: true, highmem: true, ..Default::default() }).unwrap();
    let (h, _) = adt::parse(&adt::serialize(&hi)).unwrap();
    let c = h.child("chosen").unwrap();
    let d = <[u8; 8]>::try_from(c.prop("dram-base").unwrap()).unwrap();
    assert_eq!(u64::from_le_bytes(d), mkadt::HIGH_PHYS_BASE);
    // single CPU on QEMU, 8 on the phone
    let cpus = root.child("cpus").unwrap();
    assert_eq!(cpus.children.len(), 1);
    let phone = mkadt::build(&AdtOptions::default()).unwrap();
    let (p, _) = adt::parse(&adt::serialize(&phone)).unwrap();
    assert_eq!(p.child("cpus").unwrap().children.len(), 8);
    // no UFS node under QEMU
    assert!(root.child("sweet-ufs").is_none());
    assert!(p.child("sweet-ufs").is_some());
    // boot manifest hash is sha384("sweet boot manifest")
    let h = chosen.prop("boot-manifest-hash").unwrap();
    assert_eq!(h.len(), 48);
    assert_eq!(h, mkadt::sha384(b"sweet boot manifest").to_vec());
}

#[test]
fn payload_pipeline_roundtrip() {
    // Ports the 16 checks of scripts/test_payload_pipeline.py.
    let adt_blob = adt_qemu();
    let kernel = synthetic_kernel();

    // 1. mkpayload --ramdisk
    let rd: Vec<u8> = (0..0x3000u32).map(|i| (i * 7) as u8).collect();
    let (payload, info) =
        payload::build(&kernel, &adt_blob, "debug=0xc serial=3 cpus=1 rd=md0", Some(&rd)).unwrap();

    // 2. magic/version/nsegs
    assert_eq!(&payload[0..8], b"SWEETXNU");
    assert_eq!(payload::u32(payload.as_slice(), 8), 1);
    let nsegs = payload::u32(payload.as_slice(), 12) as usize;
    assert_eq!(nsegs, 3); // __TEXT, __DATA, ramdisk

    // 3. fixups applied: the pointer in __DATA now reads its plain VA
    let text_va = info.segments[0].1;
    let data_seg = &info.segments[1];
    let data_va = data_seg.1;
    // find the pointer: it is at DATA page 0 = file offset of __DATA in payload
    let mut ptr_off = None;
    for i in 0..nsegs {
        let (vmaddr, _, foff, fsize) = payload::seg_at(&payload, i);
        if vmaddr == data_va {
            ptr_off = Some((foff + 0) as usize);
            assert!(fsize >= 8);
        }
    }
    let ptr_off = ptr_off.unwrap();
    let want = text_va + 0x2000;
    let raw = (want - text_va) & ((1u64 << 36) - 1); // fmt-6 offset from image base
    let got = payload::u64(payload.as_slice(), ptr_off);
    assert_eq!(got, want, "chained fixup rebased to unslid VA");

    // 4. ramdisk registered in ADT memory-map
    let (root, _) = adt::parse({
        let adt_off = payload::u64(payload.as_slice(), 0x20) as usize;
        let adt_size = payload::u64(payload.as_slice(), 0x28) as usize;
        &payload[adt_off..adt_off + adt_size]
    })
    .unwrap();
    let mmap = root.child("chosen").unwrap().child("memory-map").unwrap();
    let rdprop = mmap.prop("RAMDisk").expect("RAMDisk in memory-map");
    let (rd_pa, rd_sz) = (
        u64::from_le_bytes(rdprop[..8].try_into().unwrap()),
        u64::from_le_bytes(rdprop[8..].try_into().unwrap()),
    );
    assert_eq!(rd_sz as usize, (rd.len() + 0x3fff) & !0x3fff);
    assert_eq!(info.ramdisk, Some((rd_pa, rd_sz)));

    // 5. build_image places segments + ADT + boot_args; stub jumps to entry
    let placed = payload::build_image(&payload).unwrap();
    assert_eq!(placed.entry_pa, placed.phys_base + (info.entry - info.virt_base));
    // boot_args bytes: revision/version 2/2, fb 0x9c000000, 1080x2400 depth 32
    let ba_off = (placed.boot_args_pa - placed.phys_base) as usize;
    let ba = &placed.image[ba_off..ba_off + 736];
    assert_eq!(u16::from_le_bytes(ba[0..2].try_into().unwrap()), 2);
    assert_eq!(u16::from_le_bytes(ba[2..4].try_into().unwrap()), 2);
    assert_eq!(payload::u64(ba, 0x28), 0x9c000000);
    assert_eq!(payload::u64(ba, 0x38), 1080 * 4);
    assert_eq!(payload::u64(ba, 0x40), 1080);
    assert_eq!(payload::u64(ba, 0x48), 2400);
    assert_eq!(payload::u64(ba, 0x50), 32);
    assert_eq!(payload::u64(ba, 0x60), info.kernel_end); // adt VA = kernel_end
    // cmdline landed
    let cl = &ba[0x6c..0x6c + 40];
    eprintln!("cmdline bytes: {cl:?}");
    assert!(cl.starts_with(b"debug=0xc serial=3 cpus=1 rd=md0"));
    // ramdisk bytes present in the RAM image
    let rd_img_off = (rd_pa - placed.phys_base) as usize;
    assert_eq!(placed.image[rd_img_off], (0u8));
    assert_eq!(placed.image[rd_img_off + 3], 21);
    // stub encoding
    let st = payload::stub(placed.boot_args_pa, placed.entry_pa);
    assert_eq!(&st[0..4], &0x5800_0080u32.to_le_bytes());
    assert_eq!(payload::u64(&st, 16), placed.boot_args_pa);
    assert_eq!(payload::u64(&st, 24), placed.entry_pa);

    // 6. auto -m sizing (ceil((0xb0000000+0x4cf00000-0x40000000)/2^30) = 4,
    //    matching python's -(-x // 2^30) then max(4, ..))
    let gib = (placed.phys_base + placed.mem_size - 0x4000_0000).div_ceil(1 << 30);
    assert_eq!(gib.max(4), 4);

    // 7. payload_data reads the (now rebased) pointer by VA
    let probe = payload::payload_data(&payload, data_va, 8).unwrap();
    assert_eq!(&probe[..8], &want.to_le_bytes());
}

#[test]
fn im4p_lzfse_roundtrip_through_apple_c() {
    // real Apple LZFSE encode -> decode of structured data
    let data: Vec<u8> = b"hello sweet lzfse ".repeat(4096);
    let enc = im4p::lzfse_encode(&data).unwrap();
    let dec = im4p::lzfse_decode(&enc).unwrap();
    assert_eq!(dec, data);
    // IM4P wrap (bvx2 magic on compressed -> auto decode in unwrap)
    let mut im4p: Vec<u8> = Vec::new();
    im4p.extend_from_slice(&[0x30, 0x82, 0, 0]);
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(&[0x04, 0x04, b'I', b'M', b'4', b'P']);
    body.extend_from_slice(&[0x04, 0x04, b'K', b'R', b'N', b'L']);
    body.extend_from_slice(&[0x04, 0x02, 0x41, 0x42]);
    body.extend_from_slice(&[0x04, 0x82]);
    let len = enc.len();
    body.extend_from_slice(&(len as u16).to_be_bytes());
    body.extend_from_slice(&enc);
    // SEQUENCE header: 30 82 <content-length as u16 BE>
    im4p[2..4].copy_from_slice(&(body.len() as u16).to_be_bytes());
    im4p.extend_from_slice(&body);
    // Note: the compressed payload does not start with bvx2 here (lzfse_encode
    // emits bvx/bvzn blocks), so unwrap returns it as-is; decrypt flag false.
    let (p, info) = im4p::unwrap(&im4p).unwrap();
    assert_eq!(info.kind, "KRNL");
    // the lzfse encoder emits bvx2 blocks, so unwrap transparently decodes
    assert_eq!(p, data);
    assert!(im4p::looks_im4p(&im4p));
}

#[test]
fn png_roundtrip_and_fb_compose() {
    // 4x4 red image -> encode -> decode -> same pixels
    let mut img = crate::png::Image::new(4, 4);
    for px in img.rgba.chunks_mut(4) {
        px.copy_from_slice(&[255, 0, 0, 255]);
    }
    let rgb: Vec<u8> = img.rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let png = crate::png::encode_rgb(4, 4, &rgb).unwrap();
    let back = crate::png::decode(&png).unwrap();
    assert_eq!(back.pixel(0, 0), [255, 0, 0, 255]);
    assert_eq!(back.pixel(3, 3), [255, 0, 0, 255]);

    // logo composition: black canvas + centered white square logo
    let mut logo = crate::png::Image::new(8, 8);
    for px in logo.rgba.chunks_mut(4) {
        px.copy_from_slice(&[255, 255, 255, 255]);
    }
    let fb = crate::fblogo::compose(&logo, 20);
    assert_eq!(fb.len(), (1080 * 4 * 2400) as usize);
    let frac = crate::fblogo::nonblack_fraction(&fb);
    assert!(frac > 0.0 && frac < 0.1, "logo covers a small fraction: {frac}");
    let bright = crate::fblogo::bright_fraction(&fb, 200);
    assert!(bright > 0.01, "center box shows the logo: {bright}");
    // BGRA conversion: white pixel -> B=G=R=255 in fb
    let center = ((1200 - 4) * 1080 * 4 + 540 * 4) as usize;
    assert_eq!(&fb[center..center + 3], &[255, 255, 255]);
    // fb->png roundtrip
    let png2 = crate::png::encode_fb(1080, 2400, &fb).unwrap();
    let back = crate::png::decode(&png2).unwrap();
    assert_eq!(back.width, 1080);
    assert_eq!(back.pixel(540, 1200), [255, 255, 255, 255]);
}

#[test]
fn bootimg_v2_layout() {
    let kernel = vec![1u8; 300];
    let ramdisk = cpio::empty_cpio().unwrap();
    assert!(!ramdisk.is_empty());
    let dtb = vec![9u8; 77];
    let opts = bootimg::BootImageOptions {
        kernel: &kernel,
        ramdisk: &ramdisk,
        dtb: Some(&dtb),
        ..Default::default()
    };
    let img = bootimg::build_v2(&opts).unwrap();
    assert_eq!(&img[0..8], b"ANDROID!");
    let ksize = u32::from_le_bytes(img[8..12].try_into().unwrap());
    assert_eq!(ksize, 300);
    assert_eq!(u32::from_le_bytes(img[16..20].try_into().unwrap()), ramdisk.len() as u32);
    assert_eq!(u32::from_le_bytes(img[40..44].try_into().unwrap()), 2); // header_version
    // os_version 17.0.0 @2035-06 = (17<<14) | ((35<<4)|6)<<16
    let osv = u32::from_le_bytes(img[44..48].try_into().unwrap());
    assert_eq!(osv, (17 << 14) | (((35 << 4) | 6) << 16));
    assert_eq!(u32::from_le_bytes(img[1644..1648].try_into().unwrap()), 1660);
    assert_eq!(u32::from_le_bytes(img[1648..1652].try_into().unwrap()), 77);
    // page-aligned placement
    assert_eq!(img.len() % 4096, 0);
    assert!(img.len() >= 4096 + 4096 + 4096 + 4096);
}

#[test]
fn demo_kernel_parses_and_places() {
    let km = demo::macho(0x30c040, 400).unwrap();
    let m = crate::macho::parse(&km).unwrap();
    assert_eq!(m.cputype, crate::macho::CPU_TYPE_ARM64);
    assert_eq!(m.segments[0].name, "__TEXT");
    assert_eq!(m.segments[0].fileoff, 0);
    assert!(m.entry != 0);
    // payload with the demo kernel + qemu ADT: full pipeline
    let adt_blob = adt_qemu();
    let (p, info) =
        payload::build(&km, &adt_blob, "debug=0xc serial=3 cpus=1 rd=md0", None).unwrap();
    assert_eq!(info.segments[0].0, "__TEXT");
    assert!(info.entry > 0);
    let placed = payload::build_image(&p).unwrap();
    assert_eq!(placed.image.len() % 0x4000, 0);
}

#[test]
fn hello_screen_renders_and_kernel_places() {
    use crate::hello::{parse_strokes, render_strokes, SERIAL_MARKER};
    // synthetic strokes JSON in the exact CursiveHello.json schema (the real
    // file ships inside the IPSW and is pulled at runtime; the repo carries
    // no Apple content). A cursive-ish squiggle: two strokes, three segs.
    let doc = r#"{"strokes":[[
        {"p0":[0,300,30,30],"p1":[60,120,30,30],"p2":[140,60,30,30],"p3":[260,300,30,30]},
        {"p0":[260,300,30,30],"p1":[380,540,26,26],"p2":[520,560,22,22],"p3":[660,360,18,18]}
    ],[
        {"p0":[700,180,26,26],"p1":[860,180,26,26],"p2":[860,540,26,26],"p3":[700,540,26,26]}
    ]]}"#;
    let strokes = parse_strokes(doc.as_bytes()).unwrap();
    assert_eq!(strokes.len(), 2);
    assert_eq!(strokes[0].len(), 2);
    let fb = render_strokes(&strokes);
    assert_eq!(fb.len(), (crate::FB_STRIDE * crate::FB_H) as usize);
    let bright = crate::fblogo::bright_fraction(&fb, 200);
    assert!(bright > 0.01, "hello glyphs should light the center box: {bright}");
    assert_eq!(&fb[0..4], &[0, 0, 0, 255]);
    // the marker string is embedded in the kernel message
    let km = demo::macho_hello(&fb).unwrap();
    assert!(
        km.windows(SERIAL_MARKER.len()).any(|w| w == SERIAL_MARKER.as_bytes()),
        "hello kernel must carry its serial marker"
    );
    // parses as a Mach-O, __TEXT spans code + blob, payload pipeline accepts it
    let m = crate::macho::parse(&km).unwrap();
    assert_eq!(m.segments[0].name, "__TEXT");
    assert!(m.segments[0].vmsize >= km.len() as u64);
    let adt_blob = adt_qemu();
    let (p, info) =
        payload::build(&km, &adt_blob, "debug=0xc serial=3 cpus=1 rd=md0", None).unwrap();
    assert!(info.entry > 0);
    let placed = payload::build_image(&p).unwrap();
    assert!(placed.image.len() >= km.len());
    assert!(km.len() < (1 << 20) * 4096);
    // the FB analyzer must separate hello from the dithered logo
    assert!(crate::fblogo::is_hello_screen(&fb), "render_strokes must classify as hello");
    let logo_fb = crate::fblogo::blank_canvas();
    assert!(!crate::fblogo::is_hello_screen(&logo_fb), "blank canvas is not hello");
}

#[test]
fn qmp_dual_transport_against_fake_server() {
    // Port of test_qmp_branches.py: a tiny fake QMP server; unix socket here
    // (the TCP branch is covered by the same protocol logic + CI on Windows).
    use std::io::{BufRead, BufReader, Write};

    #[cfg(unix)]
    {
        let dir = std::env::temp_dir().join(format!("sweet-qmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("mon.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            let mut line = String::new();
            w.write_all(b"{\"QMP\": {\"version\": {}}}\n").unwrap();
            w.flush().unwrap();
            r.read_line(&mut line).unwrap(); // qmp_capabilities
            w.write_all(b"{\"return\": {}}\n").unwrap();
            w.flush().unwrap();
            line.clear();
            r.read_line(&mut line).unwrap(); // human-monitor-command stop
            assert!(line.contains("\"stop\""));
            w.write_all(b"{\"return\": \"\"}\n").unwrap();
            w.flush().unwrap();
        });

        let mut q = qemu::Qmp::connect(&qemu::Transport::Unix(sock)).unwrap();
        q.handshake().unwrap();
        let r = q.hmp("stop").unwrap();
        assert_eq!(r, "");
        server.join().unwrap();
        let _ = std::fs::remove_file(dir.join("mon.sock"));
    }
    #[cfg(not(unix))]
    {
        // Windows path uses TCP: exercise the protocol via std TcpListener.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let (s, _) = listener.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            let mut line = String::new();
            w.write_all(b"{\"QMP\": {}}\n").unwrap();
            w.flush().unwrap();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"return\": {}}\n").unwrap();
            w.flush().unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"return\": \"\"}\n").unwrap();
            w.flush().unwrap();
        });
        let mut q =
            qemu::Qmp::connect(&qemu::Transport::Tcp("127.0.0.1".into(), port)).unwrap();
        q.handshake().unwrap();
        assert_eq!(q.hmp("stop").unwrap(), "");
        server.join().unwrap();
    }
}

#[test]
fn jsonlite_roundtrip() {
    let mut o = J::obj();
    o.set("a", J::n(1.0));
    o.set("b", J::s("x\"y\n"));
    o.set("c", J::b(true));
    let s = o.dump();
    let back = J::parse(&s).unwrap();
    assert_eq!(back, o);
}

#[test]
fn screen_decoder_and_verdict() {
    // pe_serial.c 2x white-on-black: glyph byte c = one font row.
    // Give every printable char a distinct glyph (rows filled with c).
    let mut font = vec![0u8; 256 * 16];
    for c in 32..127u8 {
        for y in 0..16 {
            font[c as usize * 16 + y] = c;
        }
    }
    // fb full of 0xff cells: 0xff is not a printable glyph -> unknown
    let fb = vec![0xffu8; (1080 * 4 * 2400) as usize];
    let (screen, unknown) = qemu::decode_screen(&fb, &font);
    assert!(unknown > 0, "0xff cells decode as unknown");
    let r = qemu::screen_result(&screen, unknown);
    match r {
        J::Obj(m) => assert_eq!(m.get("status"), Some(&J::s("incomplete_decode"))),
        _ => panic!(),
    }
    // an fb of 'A'-glyph cells (0x41) decodes exactly
    let a_fb: Vec<u8> = (0..(1080 * 4 * 2400) as usize)
        .map(|i| if (i / 4) % 4 == 3 { 0xff } else { 0x41 }) // BGRA white-ish 'A' rows
        .collect();
    // every glyph cell reads 16 rows x 8 px; all-white px = bit set.
    // 0x41 bytes are not 0xffffffff px so cells are unknown; instead test a
    // crafted 0xffffffff fb with an 'A'-glyph font:
    let mut font2 = vec![0u8; 256 * 16];
    for y in 0..16 {
        font2[65 * 16 + y] = 0xff; // 'A' = fully set glyph
    }
    let white = vec![0xffu8; (1080 * 4 * 2400) as usize];
    let (s2, u2) = qemu::decode_screen(&white, &font2);
    assert!(s2.lines().next().unwrap().starts_with('A'));
    assert_eq!(u2, 0);
    let r2 = qemu::screen_result(&s2, u2);
    match r2 {
        J::Obj(m) => assert_eq!(m.get("status"), Some(&J::s("no_panic_observed"))),
        _ => panic!(),
    }
    let panic_screen = "panic(cpu 0 caller 0x...)\n".to_string();
    let r3 = qemu::screen_result(&panic_screen, 0);
    match r3 {
        J::Obj(m) => assert_eq!(m.get("status"), Some(&J::s("panic"))),
        _ => panic!(),
    }
}



#[test]
fn ibootim_sszl_roundtrip() {
    // synthetic grey (darkness-mask) logo: 255 = background, 12 = drawn
    let mut px = vec![255u8; 40 * 30];
    for y in 10..20 {
        for x in 12..28 {
            px[y * 40 + x] = 12;
        }
    }
    let cont = crate::ibootim::encode_grey(40, 30, &px);
    assert!(crate::ibootim::looks_ibootim(&cont));
    let img = crate::ibootim::decode(&cont).unwrap();
    assert_eq!((img.width, img.height), (40, 30));
    assert_eq!(img.format, "grey");
    // darkness 12 -> luminance 243 drawn white; bg 255 -> black
    let i = img.to_image();
    assert_eq!(i.pixel(15, 15), [243, 243, 243, 255]);
    assert_eq!(i.pixel(1, 1), [0, 0, 0, 255]);
    // sszl literal-only encode/decode roundtrip on pseudo-random data
    let data: Vec<u8> = (0..5000u32).map(|i| (i * 31 + 7) as u8).collect();
    let enc = crate::ibootim::sszl_encode_literals(&data);
    let dec = crate::ibootim::sszl_decode(&enc, data.len()).unwrap();
    assert_eq!(dec, data);
}

#[test]
fn interactive_kernel_no_unpatched_branches() {
    // Regression: a hand-assembler patch once landed one slot past its
    // placeholder, leaving a literal 0x00000000 (UDF) in the poll loop —
    // the guest faulted into a vector storm at boot.
    let km = crate::demo::macho_interactive(0x30c040, 60).unwrap();
    // walk every 4-byte word of __text; only the message bytes may be
    // non-code, and no zero word may exist inside the code region
    let code_off = 0x400usize;
    let code_len = {
        // __text section size = code bytes; find the message start
        let msg = b"\nSWEET-RUST interactive kernel OK";
        let mut at = None;
        for i in 0..km.len() - msg.len() {
            if &km[i..i + msg.len()] == &msg[..] {
                at = Some(i);
                break;
            }
        }
        let at = at.expect("banner message missing");
        at - code_off
    };
    let mut i = code_off;
    while i < code_off + code_len {
        let w = u32::from_le_bytes([km[i], km[i + 1], km[i + 2], km[i + 3]]);
        assert_ne!(w, 0, "unpatched placeholder at code offset {:#x}", i - code_off);
        i += 4;
    }
    // the banner printer must read the MESSAGE (x1), not the UART (x0):
    // ldrb w2, [x1] encodes rn=1 -> ...0x22, [x0] would be ...0x02
    let printer = &km[code_off + 0x64..code_off + 0x68];
    assert_eq!(printer, &[0x22, 0x00, 0x40, 0x39], "banner ldrb must be [x1]");
    // the FR poll must read x6 (rn=6 -> 0xc5); the adr x7 (banner reprint)
    // sits at 0x8c, so the poll ldr lands at 0x90
    let poll = &km[code_off + 0x90..code_off + 0x94];
    assert_eq!(poll, &[0xc5, 0x00, 0x40, 0xb9], "poll ldr must be [x6]");
}

#[test]
fn macho_fat_slice_and_dylibs() {
    // FAT Mach-O with one arm64 slice carrying two LC_LOAD_DYLIB.
    // fat header: magic(BE) 0xcafebabe, nfat_arch=1, arch={cputype(BE) 0x0100000c,
    // cpusub 0, offset 0x20(32B hdr), size, align}
    let mut inner: Vec<u8> = Vec::new();
    inner.extend_from_slice(&0xfeedfacfu32.to_le_bytes()); // MH_MAGIC_64
    inner.extend_from_slice(&0x0100_000cu32.to_le_bytes()); // CPU_TYPE_ARM64
    inner.extend_from_slice(&0u32.to_le_bytes());
    inner.extend_from_slice(&0u32.to_le_bytes()); // filetype
    inner.extend_from_slice(&3u32.to_le_bytes()); // ncmds: segment + 2 dylibs
    inner.extend_from_slice(&0u32.to_le_bytes()); // sizeofcmds
    inner.extend_from_slice(&0u32.to_le_bytes()); // flags
    inner.extend_from_slice(&0u32.to_le_bytes()); // reserved
    // one minimal LC_SEGMENT_64 (__TEXT, vm 0, size 0x1000)
    let mut seg = Vec::new();
    seg.extend_from_slice(&0x19u32.to_le_bytes());
    seg.extend_from_slice(&72u32.to_le_bytes());
    seg.extend_from_slice(b"__TEXT\0\0\0\0\0\0\0\0\0\0");
    seg.extend_from_slice(&0u64.to_le_bytes()); // vmaddr
    seg.extend_from_slice(&0x1000u64.to_le_bytes()); // vmsize
    seg.extend_from_slice(&0u64.to_le_bytes()); // fileoff
    seg.extend_from_slice(&0x1000u64.to_le_bytes()); // filesize
    seg.extend_from_slice(&0x07u32.to_le_bytes()); // maxprot
    seg.extend_from_slice(&0x05u32.to_le_bytes()); // initprot
    seg.extend_from_slice(&1u32.to_le_bytes()); // nsects
    seg.extend_from_slice(&0u32.to_le_bytes()); // flags
    inner.extend_from_slice(&seg);
    // LC_LOAD_DYLIB "/usr/lib/libSystem.B.dylib"
    let name = b"/usr/lib/libSystem.B.dylib\0";
    let cmdsize = (24 + name.len()) as u32;
    let mut ld = Vec::new();
    ld.extend_from_slice(&0xcu32.to_le_bytes());
    ld.extend_from_slice(&cmdsize.to_le_bytes());
    ld.extend_from_slice(&24u32.to_le_bytes()); // name offset
    ld.extend_from_slice(&0u32.to_le_bytes()); // timestamp
    ld.extend_from_slice(&0u32.to_le_bytes()); // current_version
    ld.extend_from_slice(&0u32.to_le_bytes()); // compatibility_version
    ld.extend_from_slice(name);
    inner.extend_from_slice(&ld);
    // a second dylib (weak)
    let name2 = b"/System/Library/Frameworks/Foo.framework/Foo\0";
    let mut ld2 = Vec::new();
    ld2.extend_from_slice(&0x8000_0018u32.to_le_bytes());
    ld2.extend_from_slice(&(24 + name2.len() as u32).to_le_bytes());
    ld2.extend_from_slice(&24u32.to_le_bytes());
    ld2.extend_from_slice(&0u32.to_le_bytes());
    ld2.extend_from_slice(&0u32.to_le_bytes());
    ld2.extend_from_slice(&0u32.to_le_bytes());
    ld2.extend_from_slice(name2);
    inner.extend_from_slice(&ld2);

    let mut fat: Vec<u8> = Vec::new();
    fat.extend_from_slice(&0xcafebabEu32.swap_bytes().to_le_bytes()); // 0xcafebabe BE
    fat.extend_from_slice(&1u32.swap_bytes().to_le_bytes()); // nfat_arch
    fat.extend_from_slice(&0x0100_000cu32.swap_bytes().to_le_bytes());
    fat.extend_from_slice(&0u32.swap_bytes().to_le_bytes());
    fat.extend_from_slice(&32u32.swap_bytes().to_le_bytes()); // offset
    fat.extend_from_slice(&(inner.len() as u32).swap_bytes().to_le_bytes());
    fat.extend_from_slice(&12u32.swap_bytes().to_le_bytes()); // align
    assert_eq!(fat.len(), 28);
    fat.extend(std::iter::repeat(0).take(4)); // pad to 32
    fat.extend_from_slice(&inner);

    let (m, off) = crate::macho::parse_best(&fat).unwrap();
    assert_eq!(off, 32);
    assert_eq!(m.cputype, 0x0100_000c);
    assert_eq!(m.segments.len(), 1);
    assert_eq!(m.segments[0].name, "__TEXT");
    assert_eq!(m.dylibs.len(), 2);
    assert_eq!(m.dylibs[0], "/usr/lib/libSystem.B.dylib");
    assert_eq!(m.dylibs[1], "/System/Library/Frameworks/Foo.framework/Foo");
    // thin files also parse via parse_best
    let (m2, off2) = crate::macho::parse_best(&inner).unwrap();
    assert_eq!(off2, 0);
    assert_eq!(m2.dylibs.len(), 2);
}

#[test]
fn plist_roundtrip_real_launchd() {
    // real /System/Library/xpc/launchd.plist from the 16.7.16 IPSW
    let path = std::path::Path::new("/tmp/research/launchd.plist");
    if !path.exists() {
        return; // asset not present in this environment
    }
    let raw = std::fs::read(path).unwrap();
    let p = match crate::plist::parse(&raw) { Ok(p) => p, Err(e) => panic!("parse failed: {e}") };
    let d = p.as_dict().unwrap();
    let daemons = d
        .get("LaunchDaemons")
        .and_then(|x| x.as_dict())
        .expect("LaunchDaemons dict");
    assert!(daemons.len() > 300, "expected ~491 daemons, got {}", daemons.len());
    // serialize + reparse must preserve the daemon count
    let out = crate::plist::write(&p).unwrap();
    std::fs::write("/tmp/research/launchd.rewritten.plist", &out).unwrap();
    let p2 = crate::plist::parse(&out).unwrap();
    let d2 = p2.as_dict().unwrap();
    let daemons2 = d2["LaunchDaemons"].as_dict().unwrap();
    assert_eq!(daemons.len(), daemons2.len());
    // spot-check one job dict
    let first = daemons.iter().next().unwrap();
    let job2 = daemons2.get(first.0).expect("same job in rewritten plist");
    assert_eq!(first.1, job2);
}

#[test]
fn hfs_catalog_dev_mountpoint() {
    // M3 regression: the real rootfs /dev is an EMPTY directory; the reader's
    // old "has children" rule misclassified it as a file, mkiosroot dropped
    // it, XNU devfs_kernel_mount("/dev") failed and launchd died with
    // "failed to reserve stdin descriptor: /dev/null: 2".
    // The writer must register empty dirs and report them via catalog().
    let dir = std::env::temp_dir().join("sweet-hfs-catalog-test");
    let _ = std::fs::create_dir_all(&dir);
    let img_path = dir.join("cat-test.img");
    let mut img = crate::hfs::HfsImage::create(&img_path, 64).unwrap();
    let dev = img.add_dir(crate::hfs::ROOT_ID, "dev", 0, 0, 0o755).unwrap();
    assert!(dev > 0);
    let private = img.add_dir(crate::hfs::ROOT_ID, "private", 0, 0, 0o755).unwrap();
    let etc = img.add_dir(private, "etc", 0, 0, 0o755).unwrap();
    let ld = img.add_dir(etc, "LaunchDaemons", 0, 0, 0o755).unwrap();
    let _ = img
        .add_file(ld, "com.apple.backboardd.plist", 0, 0, 0o644, 8, &mut |s, off, len| {
            s.write_file_chunk(off, b"<plist/>")
        })
        .unwrap();
    let cat = img.catalog();
    let dev_e = cat.iter().find(|e| e.path == "/dev").expect("/dev in catalog");
    assert!(dev_e.is_dir, "/dev must classify as a directory");
    let ld_e = cat
        .iter()
        .find(|e| e.path == "/private/etc/LaunchDaemons")
        .expect("nested dir path");
    assert!(ld_e.is_dir);
    let pl = cat
        .iter()
        .find(|e| e.path == "/private/etc/LaunchDaemons/com.apple.backboardd.plist")
        .expect("nested file path");
    assert!(!pl.is_dir && pl.size == 8);
    // finish() must not clobber the catalog contents
    img.finish("cat").unwrap();
    let _ = std::fs::remove_file(&img_path);
}

/// LZBITMAP (decmpfs algo 14) round-trip through the vendored clean-room
/// codec: zbm_compress (C) produces a complete stream (magic + sub-chunks +
/// terminator); the algo-14 path must unwrap it from the decmpfs offset
/// table and reproduce the input byte-exactly — both the ZBM form and the
/// raw-stored form (flag byte 0xF? + plain bytes) that Apple's compressor
/// emits for incompressible chunks (observed on the real dyld sub-caches).
#[test]
fn decmpfs_algo14_zbitmap_roundtrip() {
    extern "C" {
        fn zbm_compress(
            dest: *mut u8,
            dest_size: usize,
            src: *const u8,
            src_size: usize,
            out_len: *mut usize,
        ) -> i32;
        fn zbm_decompress(
            dest: *mut u8,
            dest_size: usize,
            src: *const u8,
            src_size: usize,
            out_len: *mut usize,
        ) -> i32;
    }
    // pseudo-random but deterministic payload > 32K so the stream holds
    // several sub-chunks, with runs of zeros like real linkedit data
    let mut src = vec![0u8; 100_000];
    let mut x = 0x12345678u32;
    for i in (0..src.len()).step_by(7) {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        src[i] = (x >> 24) as u8;
    }

    // compress (query size first, then encode)
    let mut cap = 0usize;
    let rc = unsafe { zbm_compress(std::ptr::null_mut(), 0, src.as_ptr(), src.len(), &mut cap) };
    assert_eq!(rc, 0, "zbm_compress size query failed");
    let mut stream = vec![0u8; cap];
    let mut clen = 0usize;
    let rc = unsafe { zbm_compress(stream.as_mut_ptr(), stream.len(), src.as_ptr(), src.len(), &mut clen) };
    assert_eq!(rc, 0, "zbm_compress failed");
    stream.truncate(clen);
    // a zbm stream of the whole buffer must decode whole (direct FFI check)
    let mut whole = vec![0u8; src.len() + 64];
    let mut out_len = 0usize;
    let rc = unsafe {
        zbm_decompress(whole.as_mut_ptr(), whole.len(), stream.as_ptr(), clen, &mut out_len)
    };
    assert_eq!(rc, 0, "direct zbm_decompress failed");
    assert_eq!(out_len, src.len());
    assert_eq!(&whole[..out_len], &src[..], "direct zbm round-trip mismatch");

    // wrap as decmpfs algo-14 resource fork the way Apple lays it out:
    // each decmpfs chunk (<= 64K output) is its OWN independent zbm stream;
    // table = [off0 = table end, off1, off2...], streams concatenated after.
    let mut streams: Vec<Vec<u8>> = Vec::new();
    for piece in src.chunks(0x10000) {
        let mut c = 0usize;
        let rc = unsafe { zbm_compress(std::ptr::null_mut(), 0, piece.as_ptr(), piece.len(), &mut c) };
        assert_eq!(rc, 0);
        let mut buf = vec![0u8; c];
        let mut n = 0usize;
        let rc = unsafe { zbm_compress(buf.as_mut_ptr(), buf.len(), piece.as_ptr(), piece.len(), &mut n) };
        assert_eq!(rc, 0);
        buf.truncate(n);
        streams.push(buf);
    }
    let mut rsrc = vec![0u8; 4 * (streams.len() + 1)];
    let table_end = (4 * (streams.len() + 1)) as u32;
    // off[0] = table end = chunk 0 start; off[i+1] = end of chunk i
    rsrc[..4].copy_from_slice(&table_end.to_le_bytes());
    let mut pos = table_end;
    for (i, s) in streams.iter().enumerate() {
        pos += s.len() as u32;
        rsrc[(i + 1) * 4..(i + 2) * 4].copy_from_slice(&pos.to_le_bytes());
    }
    for s in &streams {
        rsrc.extend_from_slice(s);
    }
    let got = crate::apfs::decompress_for_test(14, src.len(), &[], Some(&rsrc)).unwrap();
    assert_eq!(got, src, "zbm stream round-trip mismatch");

    // raw-stored chunks (flag byte 0xF? + uncompressed bytes): the real
    // files show these tile at 64K just like ZBM streams
    let raw_src: Vec<u8> = (0..70_000u32).map(|i| (i * 2654435761 >> 24) as u8).collect();
    let pieces: Vec<&[u8]> = raw_src.chunks(0x10000).collect();
    let mut rsrc2 = vec![0u8; 4 * (pieces.len() + 1)];
    let table_end2 = (4 * (pieces.len() + 1)) as u32;
    rsrc2[..4].copy_from_slice(&table_end2.to_le_bytes());
    let mut pos2 = table_end2;
    for (i, p) in pieces.iter().enumerate() {
        pos2 += 1 + p.len() as u32; // flag byte + payload
        rsrc2[(i + 1) * 4..(i + 2) * 4].copy_from_slice(&pos2.to_le_bytes());
    }
    for p in &pieces {
        rsrc2.push(0xFF);
        rsrc2.extend_from_slice(p);
    }
    let got2 = crate::apfs::decompress_for_test(14, raw_src.len(), &[], Some(&rsrc2)).unwrap();
    assert_eq!(got2, raw_src, "raw chunk round-trip mismatch");
}
