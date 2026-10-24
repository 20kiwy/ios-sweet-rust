# qemu-sweet — QEMU 10.0.0 + the T8015 display-path device models

This directory is the **Class B "QEMU-side world"** for the M4/M5 milestone
(docs/RESEARCH-ALL-LIBS.md §3 rows 10–11, docs/RESEARCH-SPRINGBOARD.md §3b):
a small, honest QEMU fork that adds the three Apple devices the real iOS
kernel's display drivers probe for, at the exact D22AP (iPhone X) addresses
taken from the real DeviceTree (`docs/research/devicetree-d22ap.md`):

| device (this fork) | ADT node | compatible (real kext personality) | MMIO |
|---|---|---|---|
| `sweet-dart` | `/arm-io/dart-disp0` | `dart,t8015` → `AppleT8015DART` (+ `IODARTFamily` for the `iommu-mapper` child) | main `0x18704000` size `0x4000`, remap `0x18700000` size `0x4000` |
| `sweet-mipi-dsim` | `/arm-io/mipi-dsim` | `mipi-dsim-1,synopsys` → `AppleSynopsysMIPIDSIController` | `0x18400000` size `0x100000` |
| `sweet-disp0` | `/arm-io/disp0` | `disp0,t8015` → `IOMobileFramebuffer` stack / `AppleMobileDispH10P` | main `0x18000000` size `0x2a6000`, aux `0x18708000`, pmgr-a `0x320b8000`, pmgr-b `0x3230c000` (each `0x1000`) |

All four disp0 ranges and both DART ranges are **below guest RAM**
(0x40000000), so the devices self-map into the system address space
(priority 1 — beats `virt`'s empty pcie-mmio alias container) and **no
change to `hw/arm/virt.c` is needed**.

## Stage-1 scope (deliberately honest)

The exact t8015 register semantics are reverse-engineering grade. Stage 1
does NOT pretend to be bit-accurate. It provides:

* **store+echo register files** with a handful of curated semantics
  (DART per-SID `TCR`/`TTBR0` at the old-DART strides, write-1-to-clear
  interrupt status, DWC-DSI version/PHY-status constants) so driver probes
  and read-back checks get plausible answers instead of bus errors;
* a **real IOMMU address space**: `sweet-disp0` links to `sweet-dart` and
  does its DMA through the DART's `IOMMUMemoryRegion` (stage-1 translation
  is bypass/pass-through; an enabled-but-unconfigured DART *denies* DMA and
  logs a fault — the deny path is real);
* **traces for every register write** via `qemu_log_mask(LOG_UNIMP)` —
  the runner boots with `-d unimp,guest_errors`, so everything lands in
  `qemu.log`. These traces are the stage-2 specification: the PTE walker
  (TTBR → L1/L2/L3 walk, `stt-idx-width=10`) gets written against what the
  real AppleT8015DART actually programs, not against guesses;
* a host-driven swap hook: HMP `sweet-disp0-swap <PA>` blits a BGRA surface
  through the DART address space into the framebuffer at `0x9c000000`
  (the same address the loader-painted logo uses). This is the
  "map the DART-translated surface to FB 0x9c000000" mechanism from the M5
  plan, in device form.

No interrupt is ever asserted in stage 1 (the drivers' interrupt
registration is deferred; matching/attach does not need it). The ADT side
of this (`make-adt --display`) deliberately omits `interrupt-parent`
pointers at the nonexistent AIC — dangling phandles are a crash risk.

## Build

```sh
sh qemu-sweet/build.sh          # source is VENDORED at qemu-sweet/qemu/ (v10.0.0)
# -> qemu-prefix-sweet/bin/qemu-system-aarch64
```

* **The QEMU v10.0.0 source ships in this repo** at `qemu-sweet/qemu/`
  (pristine upstream tag, byte-verified at vendor time, marked
  `linguist-vendored`). No network clone is needed: clone, run, done.
  `QEMU_SRC=/path/to/another/v10.0.0` still works for external clones.
* `build.sh` is idempotent: it resets every source path it touches
  (git checkout for a standalone clone; reverse-apply of the wiring patch
  for the vendored no-.git tree), re-copies `overlay/`, re-applies
  `0001-sweet-wire-device-models.patch` (meson/Kconfig/HMP wiring only —
  the device sources come from `overlay/`), configures a fresh
  `build-sweet/`, builds `qemu-system-aarch64` (aarch64-softmmu only) and
  installs it into `$QEMU_PREFIX/bin`. A hard post-apply gate fails the
  build if the wiring did not land (a silent no-op apply once produced a
  stock-QEMU binary with zero diagnostics).
* `FAST=1 sh qemu-sweet/build.sh` resumes an interrupted long build
  (reuses the configured `build-sweet/`; ninja continues incrementally).
* Sandbox (no root): the system needs `gcc`, `pkg-config`, `glib-2.0` and
  `pixman-1` development files; `tools/install-qemu-prefix.sh` shows the
  dpkg -x pattern for obtaining them without root. Meson/ninja can come
  from pip (`pip install meson ninja`).
* CI: `ubuntu-latest` + `libglib2.0-dev libpixman-1-dev zlib1g-dev libfdt-dev`
  (see `.github/workflows/qemu-m5-display.yml` — no source cache needed
  any more, the source is the checkout).

## Run

```sh
export PATH="$PWD/qemu-prefix-sweet/bin:$PATH"
sweet-rust make-adt out/m5.adt --qemu --display
# empty cpio (TRAILER!!! only): WITHOUT a RAMDisk the kernel parks in
# _cpu_idle before IOKit output (measured) — md0 must exist even empty.
python3 - <<'PY'
def hdr(ino, mode, nlink=1, mtime=0, filesize=0, dmaj=0, dmin=0, rmaj=0, rmin=0, namesize=0, chksum=0):
    return ''.join(f'{f:08X}' if isinstance(f, int) else f for f in ['070701', ino, mode, nlink, mtime, filesize, dmaj, dmin, rmaj, rmin, namesize, chksum]).encode()
name = b'TRAILER!!!\x00'
h = hdr(0, 0, namesize=len(name))
pad = lambda b, n: b + b'\x00' * ((-len(b)) % n)
open('empty.cpio', 'wb').write(h + name + pad(h + name, 4))
PY
# debug=0x8 is DB_KDB (the kernel waits for a KDB connection forever);
# the proven value is debug=0xc:
sweet-rust make-payload kernel/kernel.development.sweet out/m5.adt out/payload.bin \
  --cmdline 'debug=0xc serial=3 cpus=1 keepsyms=1 rd=md0 -noprogress wdt=-1' \
  --ramdisk empty.cpio
sweet-rust boot-qemu out/payload.bin out/m5 --secs 150 --sweet-devices
grep 'sweet-' out/m5/qemu.log      # every device register access, in order
```

Measured verdict of exactly this recipe (vendored-source build, sandbox):
all three devices realized at the D22AP bases, disp0 DMA rooted at the
DART IOMMU, boot survives IOKit matching with the display subtree and
dies at the M2 wall (`cannot mount root, errno = 19` + the `mount(20)
failed` retry loop) — see `RESEARCH-SPRINGBOARD.md` §3b.

`boot-qemu --sweet-devices` expands to:

```
-device sweet-dart,id=sweet-dart0
-device sweet-mipi-dsim
-device sweet-disp0,dart=sweet-dart0
```

(order matters: disp0 resolves the DART link at realize).

## What the run measures

1. `sweet-*: realized` — the three devices exist at the D22AP bases.
2. `sweet-disp0: DMA via DART iommu AS "sweet-disp0-dma"` — the display
   controller's DMA address space is rooted at the DART's IOMMU.
3. `sweet-dart: W TCR[n] / TTBR0[n][m]` — the kernel's real DART driver
   programming the IOMMU (the stage-2 PTE-walker spec, measured).
4. `sweet-disp0: W ...` / `sweet-mipi-dsim: W ...` — IOMFB/DSI driver
   probe traffic.
5. The boot verdict (panic or clean IOKit matching) is recorded honestly —
   `.github/workflows/qemu-m5-display.yml` reports it either way.

## Stage 2 (next, driven by the traces)

* DART PTE walker: translate = TTBR[SID] → 4-level walk of guest RAM
  (page-size 0x1000, `stt-idx-width=10` sub-TTBRs), real fault reporting,
  TLB invalidate ops honored.
* disp0 swap path keyed on the register the real IOMFB writes (from the
  traces) instead of the host-driven HMP hook.
* DSI command FIFO acks so the `lcd,summit` panel interface completes.
