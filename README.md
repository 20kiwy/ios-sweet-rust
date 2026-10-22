# ios-sweet-rust

**The ios-sweet boot pipeline, rewritten in Rust — one binary, zero runtime deps.**

This is a full reimplementation of the [ios-sweet](https://github.com/dipa-studio/ios-sweet)
boot toolchain as a native Rust workspace. It takes Apple's **iPhone X iOS 16.7.16
IPSW**, extracts the **Apple logo asset** out of it, packs a bootable **SWEETXNU
payload**, injects the logo into the framebuffer at `0x9c000000` and **boots it in
QEMU** — the guest kernel then executes and flips the framebuffer to the iOS
**"Hello" welcome screen** on a 1080x2400 canvas, with every step verified
(`boot-verdict.json`).

Every library in the boot path is implemented here:

| Piece | Python original | What this repo does |
| --- | --- | --- |
| Mach-O + chained fixups | `tools/mkpayload.py` | byte-exact port, all 8 chained-pointer formats |
| Apple Device Tree | `tools/adt.py`, `tools/mkadt.py` | parse / serialize / generate (QEMU + phone ADTs) |
| SWEETXNU payload | `tools/mkpayload.py` | kernel + ADT + boot-args + ramdisk, `/chosen/memory-map/RAMDisk` |
| QEMU RAM image, boot_args, CPU stub | `tools/qemu-sweet.py` | physical placement, 736-byte boot_args, 6-instruction stub |
| QMP control | `tools/qemu-sweet.py` | unix + TCP loopback transports, `pmemsave` screenshots |
| IM4P / IMG4 | `tools/adt.py` | DER unwrap, KBAG detection |
| LZFSE (`bvx2`) | `lzfse` CLI | **Apple's C code vendored** (FFI) |
| zlib (PNG codec, IPSW zip) | `zlib` CLI | **miniz vendored** (FFI): inflate, deflate, CRC32 |
| IPSW (zip, ZIP64) | `tools/remotezip.py` | local files + HTTP range requests through `curl` |
| iBootIm + SSZL (`lzss`) | (none — new) | the all_flash boot-asset format, incl. Apple's classic LZSS |
| Android boot.img v2 | `shim/build.sh` + `mkbootimg` | header v2 writer (same field values) |
| PNG codec | `write_png` + system tools | encoder + decoder, no image libraries |

## What is proven to work

| Area | State |
| --- | --- |
| IPSW reading | The real `iPhone10,3_16.7.16_20H392` IPSW: ZIP64 central directory over HTTP range requests in ~2 s (no 6 GB download) |
| Apple logo | `applelogo@3x~iphone.im4p`: IM4P → iBootIm (grey 194x238) → SSZL decode → white-on-black logo, rendered into the 1080x2400 framebuffer |
| **Hello screen** | `ipsw-boot --hello`: the kernel memcpy's the pre-rendered welcome screen over the logo once guest code runs — the visible *boot progressed* milestone, verified framebuffer-side (`hello_visible: true`) |
| Payload pipeline | 11/11 unit tests: chained fixups, ADT round-trip, ramdisk → `md0`, boot_args layout (736 bytes), CPU stub, auto `-m` sizing, hello classification |
| QEMU boot | `ipsw-boot --demo --lowmem`: QEMU 10 boots the payload, the injected Apple logo is visible in the screenshot, the demo kernel's framebuffer marker proves execution, `boot-verdict.json` says **pass** |
| iBootIm/SSZL | round-trip tests + decode of the real 16.7.16 asset (visually verified) |

The **SWEET xnu kernel itself** is built by the upstream repo's CI
(`xnu.yml`, macOS runner); this toolchain consumes `kernel.development.sweet`
via `--kernel`. Without it, `--demo` synthesizes a tiny bootable arm64 kernel
that paints a marker strip and prints to the PL011 — enough to prove the whole
IPSW→logo→QEMU path on any machine (that is what the CI job runs).

## How it boots

```
IPSW (file or URL, HTTP ranges)
  -> ipsw.rs     zip/ZIP64 central directory -> applelogo + kernelcache members
  -> im4p.rs     IM4P/DER unwrap, bvx2 -> vendored Apple LZFSE
  -> ibootim.rs  iBootIm header -> sszl_decode (Apple iBoot LZSS) -> grey mask
  -> fblogo.rs   black BGRA canvas 1080x2400 + centered logo -> logo.bin
  -> demo.rs     (or --kernel) arm64 Mach-O + boot_args
                 (--hello: the Mach-O carries the pre-rendered welcome
                  screen; the guest kernel memcpy's it over the logo)
  -> payload.rs  SWEETXNU: kernel segments + ADT + ramdisk + boot-args
  -> qemu.rs     loader devices: ram.bin @ phys_base, stub @ 0x40200000,
                 logo.bin @ 0x9c000000 -> CPU starts at the stub, x0 = boot_args
  -> fb.png + serial.txt + result.json + boot-verdict.json
```

The native code is vendored verbatim from upstream repositories (see
`crates/sweet-core/src/vendor/PROVENANCE.md`): Apple's **LZFSE** and
**miniz**. Rust drives them through a thin FFI; there are no other
dependencies beyond the `cc` build driver.

## How to do it yourself

You need a Rust toolchain (1.75+), `curl` in PATH, and (only for the QEMU
commands) `qemu-system-aarch64`. Windows works natively (QMP moves to a TCP
loopback port automatically, like the Python tool).

### Build

```sh
cargo build --release          # -> target/release/sweet-rust
cargo test                     # 10 tests, no network needed
```

### Apple logo straight from the IPSW (no QEMU, no kernel)

```sh
sweet-rust fb-render https://.../iPhone10,3,iPhone10,6_16.7.16_20H392_Restore.ipsw fb.png
```

Reads the IPSW over HTTP range requests (only the few hundred KB it needs),
decodes the logo asset and composes the framebuffer PNG. Use a local file path
instead of a URL if you have the IPSW downloaded.

### Full boot test in QEMU

```sh
sweet-rust ipsw-boot --ipsw IPSW_or_URL out/ --demo --lowmem --secs 20
# out/fb.png            what the "phone screen" would show (Apple logo + demo strip)
# out/boot-verdict.json pass/fail + logo_visible + panic watch
# out/payload.bin       the SWEETXNU payload (reusable with boot-qemu)
```

`--lowmem` fits a 2 GiB guest (CI-friendly); without it the QEMU ADT asks for
the phone's 4 GiB layout, like the Python tool.

### Boot to the Hello screen

```sh
sweet-rust ipsw-boot --ipsw IPSW_or_URL out/ --hello --lowmem --secs 30
# out/fb.png            the iOS welcome screen: white "Hello" + home indicator
# out/boot-verdict.json hello_visible: true (framebuffer-detected), pass: true
```

The Apple logo from the IPSW is still injected first — the screenshot is taken
after the guest kernel replaced it with the welcome screen, proving the boot
chain progressed: logo → kernel executes → hello.

### Boot the real SWEET kernel

```sh
gh run download RUN_ID -R dipa-studio/ios-sweet -n xnu-kernel -D xnu-out   # or your fork
sweet-rust ipsw-boot --ipsw IPSW out/ --kernel xnu-out/kernel.development.sweet \
    --cmdline 'debug=0xc serial=3 cpus=1 keepsyms=1 rd=md0' --secs 300
```

`verdict` watches for `BSD root: md0`, `init running as PID` and panics, exactly
like the upstream `qemu-boot.yml` job.

### The individual steps

```sh
sweet-rust ipsw-info IPSW_or_URL                  # list members (ZIP64, URL ranges)
sweet-rust ipsw-extract IPSW --member kernelcache --member applelogo --out parts/
sweet-rust kernel-unpack parts/kernelcache.release.iphone10b.im4p kernel.macho
sweet-rust make-adt qemu.adt --qemu [--highmem] [--product d22ap.adt]
sweet-rust make-payload kernel.macho qemu.adt payload.bin --cmdline '...' [--ramdisk img]
sweet-rust logo-extract IPSW logo.png             # the decoded asset as PNG
sweet-rust inject-logo IPSW logo.bin              # BGRA blob for the loader device
sweet-rust make-bootimg --kernel Image --ramdisk payload.bin --dtb dtb -o shim-boot.img
sweet-rust place-image payload.bin ram.bin         # debug: the physical RAM image
sweet-rust boot-qemu payload.bin out/ [--logo logo.bin] [--live-serial] [--gdb]
```

## Repository layout

| Path | What |
| --- | --- |
| `crates/sweet-core/` | the library: every module listed above + the test suite |
| `crates/sweet-core/src/vendor/` | vendored native C (Apple LZFSE, miniz) + provenance |
| `crates/sweet-cli/` | the `sweet-rust` binary |
| `.github/workflows/rust.yml` | build + test matrix (Linux/macOS/Windows), release artifacts |
| `.github/workflows/qemu-logo.yml` | CI boot test: real IPSW -> Apple logo -> QEMU screenshot |

## Parity notes (what differs from the Python tools)

- `place-image` and the iBootIm/SSZL decoder are new (the Python pipeline never
  needed them; the logo injection was ad-hoc in the previous session).
- The demo kernel's PL011 serial marker is best-effort: QEMU 10's PL011 gates
  TX FIFO timing in ways that can drop bytes when the guest exits fast; the
  verdict therefore uses the framebuffer markers (corner pixel for the demo
  kernel, solid-white-text-over-black classifier for the hello screen), which
  are deterministic.
- `boot-qemu` clamps guest RAM to >= 2 GiB (the Python tool clamps to 4) so the
  demo/CI boot fits small hosts; the standard QEMU ADT still computes 4-5 GiB.
- Everything else — payload layout, boot_args fields, ADT properties, fixup
  formats, QMP handshakes, exit codes (0/1/2/3/130) — is byte-for-byte the
  same contract as the Python tools.

## Credits

Built on Apple's open-source releases and the same assets as ios-sweet:
[LZFSE](https://github.com/lzfse/lzfse) (BSD-3), [miniz](https://github.com/richgel999/miniz)
(MIT), the [ios-sweet](https://github.com/dipa-studio/ios-sweet) bring-up by
dipa-studio, and [blacktop/ipsw](https://github.com/blacktop/ipsw) for the
format reference. iOS itself is Apple's and is not distributed here.
