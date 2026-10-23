# RESEARCH — from "kernel boots" to "SpringBoard renders"

**Goal:** boot the real iOS 16 userspace (`launchd` → `backboardd` → `SpringBoard`)
from the real iPhone X 16.7.16 IPSW inside QEMU, until the real home screen is
composited by the real `SpringBoard` binary into the real framebuffer.

**Honesty rules that govern every milestone:**

1. Every screen pixel is either (a) rendered by a real IPSW binary, or (b)
   painted by the host and **labeled as host-painted**. Nothing else.
2. Every verdict field maps to a real artifact (PNG, serial log, gdb dump).
3. A milestone is "done" only when its acceptance criterion reproduces in CI.
4. If a wall is impassable, we document the wall instead of hiding it.

---

## 0. Where we are today (verified state)

| Layer | Status | Evidence |
|---|---|---|
| iBoot-stage shim + MMU setup | works | QEMU boot, payload path |
| XNU 8796.141.3 (real iOS 16 kernel) + 81 SWEET patches | boots, zero panic, 150 s | serial log, `boot-verdict.json` |
| `BSD root: md0` + `/sbin/launchd` (PID 1) | starts | serial log |
| Apple logo (real `applelogo@3x~iphone.im4p`) | on-screen 1080x2400 | `qemu-boot-apple-logo.png` |
| Hello welcome data (real `CursiveHello.json` from rootfs APFS) | rasterized to FB | repo history `5eb97c7` |
| Wallpaper (real WallpaperKit HEIC + Apple `main.caml` geometry) | composited 1125x2436→1080x2400 | `wallpaper_sha256` in `emu-verdict.json` |
| Live control plane (QMP + gdbstub + serial + shared-RAM FB) | host paints FB, types into guest UART, guest echoes on-screen | `emu-control/emu-verdict.json` |
| Rootfs browser over HTTP ranges (318,071 entries) | works | `ipsw-ls`/`ipsw-find`/`ipsw-file` |
| **backboardd / SpringBoard / IOKit userspace** | **NOT running** | — |

The wall: IOKit platform drivers in the (closed-source) kernelcache do not
find the Apple hardware they probe for on QEMU `virt`, so the platform expert
never publishes the device graph that `backboardd` and `SpringBoard` require.

---

## 1. State of the art — what exists in public (researched)

| Effort | What it proves | What we reuse |
|---|---|---|
| **Corellium** (CHARM hypervisor, commercial) | Real iOS — including full SpringBoard UI — runs on *virtual* Apple platform hardware; they restore physical-device IPSWs onto virtual devices (serial `CORELLIUM-*`). Fair-use ruling (2020, upheld 2023) establishes the legal precedent for iOS virtualization. | The existence proof + approach: emulate the Apple platform devices, not a generic PC. Their DTS work seeded mainline Linux (see next row). |
| **Mainline Linux on Apple A7–A11** (`arch/arm64/boot/dts/apple/t8010.dtsi`, `t8015-common.dtsi`; `drivers/perf: apple_m1: Add Apple A7-A11, T2 SoC support`) | The register map of *exactly our SoC generation* (T8015/A11 = iPhone X) is public and merged upstream: AIC, PMGR/pinctrl, S5L UART, WDT, GPIO. T8010 DTS explicitly credits "Asahi Linux's M1 and **Corellium's A10 efforts**". | Register bases + semantics for AIC v2, PMGR, `apple,s5l-uart`, pinctrl — the QEMU devices we must model. |
| **pongoOS** (checkra1n, public source) | A public pre-boot environment that initializes T8010/T8015 hardware on real iPhones (incl. iPhone X): clocks, DART, framebuffer drivers. | Driver reference for register programming order and DART handling on T8015. |
| **Asahi Linux** | The deepest public documentation of Apple platform device architecture (AIC, DART IOMMU, PMGR) — same architectural lineage, newer silicon. | DART/AIC implementation guidance for QEMU devices. |
| **ios-sweet** (dipa-studio, base of this repo) | Real XNU boots to userspace on stock QEMU `virt` with 81 patches. | The pipeline this repo already reimplements in Rust. |

**Conclusion:** nothing here is fantasy. Each ingredient has public precedent;
the engineering gap is (a) Apple platform devices in QEMU and (b) the IOKit
matching graph — and (a) has public register maps for this SoC generation.

---

## 2. What the IPSW itself gives us (authoritative, already accessible)

* **kernelcache** — the entire IOKit driver set (closed-source kexts) as a
  kernel collection; the kernel already boots with it.
* **Rootfs** — `backboardd`, `SpringBoard`, `IOMobileFramebuffer`, every
  framework dylib (318k entries browsable over HTTP ranges today).
* **DeviceTree (D22AP)** — the *authoritative* register map of iPhone X
  hardware: every device node the kernel probes, with base addresses.
  This, not guesswork, defines what QEMU must emulate.
* UI assets: logo, `CursiveHello.json`, WallpaperKit (already extracted).

---

## 3. The plan — milestones with acceptance criteria

### M1 — Inventory: know exactly what must exist (real data only)
Tools (this repo, Rust, run against the real IPSW):
* `adt-dump` — extract the real D22AP DeviceTree from the IPSW, parse the ADT,
  emit every device node (name, io-name, base/size) → `docs/research/devicetree-d22ap.md`.
* `macho-deps` — parse `LC_LOAD_DYLIB` of `backboardd`, `SpringBoard`,
  `IOMobileFramebuffer` from the rootfs → `docs/research/userspace-deps.md`
  (what userspace links, which IOKit frameworks it talks to).
* `kc-kexts` — walk the kernel-collection fileset entries, pull embedded
  `__info_plist`, emit kext → `IONameMatch` table → `docs/research/kc-kexts.md`.

**Acceptance:** three manifests checked in, each row traceable to real IPSW
bytes (paths + sizes recorded). This defines the full emulation surface.

### M2 — Boot instrumentation: find the first wall precisely
Boot-arg `debug=0x8` (IOKit verbose) + `kprintf` → serial; run and capture the
exact driver that fails to match/attach first on `virt`.
**Acceptance:** a real boot log artifact naming the first failing IOKit match;
committed as `docs/research/first-wall.md` + raw log.

### M3 — Platform wave 1: interrupts, time, UART
Implement QEMU devices for **AIC** (`apple,t8015-aic` — mainline binding),
**PMGR gates** (stubbed always-on first), **S5L UART**; extend the synthetic
ADT with the real nodes from M1 so the kernel's own drivers match them.
**Acceptance:** XNU boots with at least the AIC driver matching and
functioning; ioreg state (via gdbstub) shows the published node.

### M4 — DART + storage
Implement `dart-t8015` register model (translation pass-through to host RAM),
needed by display; storage stays on the existing md0 ramdisk path.
**Acceptance:** DART attaches; translated framebuffer region readable by
the kernel (verified via gdb dump).

### M5 — Display: IOMobileFramebuffer attaches
Minimal `disp0` register model (or a virtual-display patch keyed on the
IOMFB kext) mapping the DART-translated surface to FB `0x9c000000`.
**Acceptance:** `backboardd` starts and obtains a working IOMFB connection;
the first UI composition drawn *by the real backboardd* appears in `fb.png`.

### M6 — Input: HID event injection
Virtual HID event source (Apple HID event system personality); host control
plane injects touch/button events; `backboardd` routes them.
**Acceptance:** scripted injection causes a real UI state change on screen.

### M7 — SpringBoard first frame
Extend the launchd boot graph (rootfs overlay/ramdisk plist) to start
`SpringBoard`; SEP/trustd-dependent daemons fail gracefully (their needs are
listed by M1 manifests).
**Acceptance:** home screen rendered by the real `SpringBoard` binary from the
IPSW into the real framebuffer; verdict gains `springboard_visible=true`,
`rendered_by=SpringBoard` — no host-painted pixels.

---

## 3b. Milestone execution log (what actually happened)

The plan above was written before the boot experiments; the real walls came in
a different order than predicted. This log is kept because the honest map of
reality beats the original guess.

* **M2 result — the first wall was not IOKit.** The SWEET-patched kernel boots
  on QEMU virt with zero panics; IOKit matching is cleared by the existing 81
  patches. The wall was the rootfs: `md0` needs a real iOS HFS+ root
  (`cannot mount root, errno=19` retry loop). Evidence: `fb-wall.png`, real
  console text decoded via the kernel's own `_iso_font`.
* **M3 (rootfs) — built and booted.** `mkiosroot` (pure-Rust HFS+ writer,
  fsck_hfs-validated) assembles the real root from the live IPSW over HTTP;
  CI boots it as `md0` with upstream boot-args. Run 39954870630: **the root
  mounted and Apple's real launchd (from the dyld shared cache) executed**,
  then exited with its own exit reason:
  `failed to reserve stdin descriptor: /dev/null: 2` — the kernel's
  `devfs_kernel_mount("/dev")` (bsd_init.c) needs a real `/dev` DIRECTORY as
  mountpoint; `devfs_init()` then provides `/dev/null` (3,2) itself.
* **The /dev fix.** The real rootfs `/dev` is an empty directory (mode
  0o40755, verified via `ipsw-stat` on the OS volume); our APFS reader
  misclassified empty dirs as files, so the writer never created the
  mountpoint. Fixed by classifying from the inode `S_IFDIR` bits + an
  explicit VFS mountpoint set + a pre-ship boot-critical-closure verifier
  (`mkiosroot` now asserts `/dev`, `/sbin/launchd`, 491 daemon plists,
  `SystemVersion.plist`, DSC before the image ships).
* **The dyld wall — libSystem not loadable (run 39992149493, 2026-10-23).**
  With `/dev` fixed, launchd got one dyld step further and died with its own
  exit reason: `Library not loaded: /usr/lib/libSystem.B.dylib ... (no such
  file, no dyld cache)` — dyld refuses an INCOMPLETE shared cache, and
  libSystem exists only inside it on iOS. The per-file skip report (994c974)
  had already named the exact 3 skipped files: all three dyld shared cache
  sub-caches (`dyld_shared_cache_arm64.28/.42.dyldlinkedit` + `.symbols`,
  0.89 GiB), all blocked by decmpfs algo 14.
* **The LZBITMAP fix.** decmpfs algo 13/14 (Apple "LZBITMAP", used by the
  SystemOS cryptex for the DSC sub-caches) had no public decoder; Corellium
  published a clean-room MIT implementation (eafer/libzbitmap), now vendored
  like LZFSE/miniz. Format, verified against the real IPSW bytes: rsrc fork
  = u32le offset table (same layout as algo 12) + chunks; each 64K chunk is
  either a complete LZBITMAP stream (`ZBM\x09` magic, `{len u24, decmp_len
  u24}` sub-chunks of ≤32 KiB, literal + 3 metadata areas + 17-byte trailing
  bitmap table, `decmp_len=0` terminator, zero pad) or raw-stored
  (`0xF?` flag byte + plain bytes) for incompressible tails. All three
  sub-caches decode byte-exact (sizes match the decmpfs records; outputs
  begin with a genuine `dyld_v1   arm64` cache header); round-trip unit test
  added. mkiosroot no longer skips anything for algo 14: the mini root now
  carries the COMPLETE dyld shared cache, so dyld can resolve
  `/usr/lib/libSystem.B.dylib` for launchd.
* **Where this lands us:** stage = `launchd` bring-up. Each further wall is
  expected inside userland (daemon exec/dyld, Mach bootstrap services,
  `com.apple.backboard.display.services` + `com.apple.iohideventsystem` —
  the M5/M6 gates from `docs/research/userspace-deps.md`). The QEMU device
  milestones (AIC/PMGR/DART/disp0 above) stay on the map for anything the
  SWEET kernel patches do not already clear on `virt`.

---

## 4. Risks (stated plainly)

* **Scale:** Corellium took a company years. M3–M5 are the hard core; each may
  surface new register-level walls that need more reverse engineering.
* **SEP-coupled daemons:** some daemons want SEP services that cannot exist in
  QEMU; M1b's dependency manifest tells us which can be tolerated absent.
* **AMFI/codesigning:** userspace launch requires the SWEET AMFI patches to
  accept rootfs binaries — already in the 81-patch set for the kernel side;
  userspace policy must be verified per milestone.
* **Timeboxes:** every milestone gets a timebox; a missed timebox produces a
  documented wall, not fabricated success.

## 5. References

* checkra1n/pongoOS — https://github.com/checkra1n/pongoOS (T8010/T8015 pre-boot drivers, public source)
* Mainline Linux Apple DTS — `arch/arm64/boot/dts/apple/t8015-common.dtsi`, `t8010.dtsi` (torvalds/linux); "[PATCH v4 19/20] arm64: dts: apple: Add A11 devices"
* LWN — "drivers/perf: apple_m1: Add Apple A7-A11, T2 SoC support" (A7–A11 supported in mainline)
* T8010 DTS commit note: "Based on Asahi Linux's M1 (t8103.dtsi) and Corellium's A10 efforts"
* Corellium — https://www.corellium.com; virtual devices restore physical-device IPSWs (serial `CORELLIUM-*`); CHARM Arm-native hypervisor
* Asahi Linux — AIC/DART/PMGR documentation (M-series lineage of the same devices)
