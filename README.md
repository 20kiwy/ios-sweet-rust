# ios-sweet-rust

**The ios-sweet boot pipeline, rewritten in Rust — one binary, zero runtime deps.**

This is a full reimplementation of the [ios-sweet](https://github.com/dipa-studio/ios-sweet)
boot toolchain as a native Rust workspace. It takes Apple's **iPhone X iOS 16.7.16
IPSW**, extracts the **Apple logo asset** out of it, packs a bootable **SWEETXNU
payload**, injects the logo into the framebuffer at `0x9c000000` and **boots it in
QEMU** — the guest kernel then executes and flips the framebuffer to the iOS
**the real "Hello" welcome screen** on a 1080x2400 canvas, with every step verified
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
| **Apple platform device models (M4/M5)** | `qemu-sweet/` | **QEMU 10.0.0 fork**: `sweet-dart` (real IOMMU MR, DMA-deny faults), `sweet-disp0` (DMA AS rooted in the DART + HMP `sweet-disp0-swap` hook), `sweet-mipi-dsim`; register write traces in `qemu.log`; `make-adt --display` synthesizes the real D22AP display subtree; CI job `qemu-m5-display.yml` |

## What is proven to work

| Area | State |
| --- | --- |
| IPSW reading | The real `iPhone10,3_16.7.16_20H392` IPSW: ZIP64 central directory over HTTP range requests in ~2 s (no 6 GB download) |
| Apple logo | `applelogo@3x~iphone.im4p`: IM4P → iBootIm (grey 194x238) → SSZL decode → white-on-black logo, rendered into the 1080x2400 framebuffer |
| **Hello screen (real IPSW data)** | `ipsw-boot --hello`: pulls the genuine `CursiveHello.json` vector strokes out of the IPSW's sealed system volume — zip-STORED dmg → UDIF blkx chunks (lzfse/zlib via vendored Apple code) → APFS catalog walk → decmpfs transparent decompression — rasterizes SpringBoard's own cursive hello and the guest kernel memcpy's it over the logo (`hello_visible: true`, JSON sha256 in the verdict) |
| Payload pipeline | 12/12 unit tests: chained fixups, ADT round-trip, ramdisk → `md0`, boot_args layout (736 bytes), CPU stub, auto `-m` sizing, hello parser/rasterizer, SHA-256 |
| QEMU boot | `ipsw-boot --demo --lowmem`: QEMU 10 boots the payload, the injected Apple logo is visible in the screenshot, the demo kernel's framebuffer marker proves execution, `boot-verdict.json` says **pass** |
| iBootIm/SSZL | round-trip tests + decode of the real 16.7.16 asset (visually verified) |
| **Wallpaper from the IPSW** | the real iOS 16 Clownfish: 11 HEIC layers + Apple's CoreAnimation layout (main.caml) pulled out of `/Library/Wallpaper/...` in the sealed system volume, composited at 1125x2436 with Apple's own geometry, zero hand-drawn pixels |
| **Interactive emulator control** | `emu-boot`: boots a LIVE QEMU session (QMP + gdbstub + serial socket + shared-memory guest RAM); the host repaints the framebuffer with the IPSW wallpaper at runtime, types into the guest UART, and the guest ECHOES it back and paints the keystrokes on screen (`emu-verdict.json`: logo/wallpaper/input_visible + input_echoed all true) |
| Rootfs file browser | `ipsw-ls` / `ipsw-find` / `ipsw-file`: list/search/extract any real file of the IPSW system volume (318,071 indexed entries in ~16 s over HTTP ranges) |

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
                 (--hello: the Mach-O carries the welcome frame rendered
                  from the IPSW's own CursiveHello.json vector strokes;
                  the guest kernel memcpy's it over the logo)
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

### Boot to the real Hello screen

```sh
sweet-rust ipsw-boot --ipsw IPSW_or_URL out/ --hello --lowmem --secs 30
# out/fb.png              the real iOS welcome screen: cursive white "hello"
# out/CursiveHello.json   the exact vector data SpringBoard uses (from the IPSW)
# out/boot-verdict.json   hello_visible: true, hello_sha256, hello_source
```

The hello screen is **not drawn by us**: iOS 16 stores the cursive multilingual
"hello" as vector strokes (`SpringBoard.framework/<lang>.lproj/CursiveHello.json`,
36 languages) inside the sealed system volume, compressed with Apple's decmpfs
transparent compression. The pipeline parses the UDIF chunk table, walks the
APFS catalog, decompresses the file and rasterizes the exact strokes (X-mirrored
out of Apple's pen-space, stroke width from the 4th component). The Apple logo
from the IPSW is still injected first — the screenshot is taken after the guest
kernel replaced it with the hello frame, proving the boot chain progressed:
logo → kernel executes → real hello. Provenance (`hello_source` path inside the
IPSW + `hello_sha256`) is recorded in the verdict and `hello-extract` writes a
`provenance.json` next to the PNG.

Just want the asset? `sweet-rust hello-extract IPSW_or_URL outdir/ --lang en_GB`

### Live control: wallpaper + typing into the guest

`emu-boot` keeps the VM alive and drives it from the host — this is the
"controllable phone" mode:

```bash
sweet-rust emu-boot --ipsw ipsw.ipsw out-emu \
  --wallpaper wall/clownfish-1080x2400.png \
  --send "hello from the host" --secs 30
```

What happens, in order (all evidence lands in `out-emu/`):

1. QEMU boots with the injected Apple logo, an interactive demo kernel that
   POLLS the PL011 receive FIFO, a serial socket chardev, QMP, and a
   shared-memory guest RAM file (`ram.mmap`, `memory-backend-file share=on`).
2. Screenshot 1 — the Apple logo from the IPSW.
3. The host repaints the framebuffer at `0x9c000000` by writing the shared RAM
   file — Screenshot 2 shows the real iOS 16 wallpaper ON THE EMULATED PHONE.
4. The host types into the guest UART — the guest kernel ECHOES the bytes back
   (captured in `serial.txt`) and paints one yellow cell per keystroke onto
   the screen — Screenshot 3. Pressing Enter makes the guest reprint its
   banner, so the host can recover the marker even if it raced the boot log.
5. `emu-verdict.json`: `logo_visible`, `wallpaper_visible`, `input_echoed`,
   `input_visible`, `kernel_alive` — all must be true for pass.

The wallpaper itself is 100% IPSW-sourced: `ipsw-file` pulls the 11 Clownfish
HEIC layers and Apple's `main.caml` layer-geometry files out of
`/Library/Wallpaper/Collections/...` in the sealed system volume, then
`tools/compose-wallpaper.py` composites them with Apple's own CoreAnimation
transform math at 1125x2436 (375x812 pt @3x, the iPhone X panel) and resizes
to the 1080x2400 framebuffer. Provenance sha256s land in the verdict.

Inspect the guest like a phone under test:

```bash
sweet-rust emu-peek out-emu/payload.bin out-emu/peek --addr 0x48000480 --len 96
sweet-rust ipsw-ls ipsw.ipsw /Library/Wallpaper
sweet-rust ipsw-find ipsw.ipsw heic
sweet-rust ipsw-file ipsw.ipsw /Library/Wallpaper/DefaultWallpapers~iphone.plist --out dw.plist
```

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
| `.github/workflows/qemu-logo.yml` | CI boot test: real IPSW -> Apple logo -> QEMU screenshot, then the real hello screen (sha256 provenance asserted) |

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
