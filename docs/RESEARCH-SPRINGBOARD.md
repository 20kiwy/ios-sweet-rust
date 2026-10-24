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
* **The user-database wall — 'mobile' unresolved (run 40002930779, 2026-10-23).**
  With the complete DSC, the real launchd loaded libSystem from the dyld
  cache, ran as PID 1 with 5 threads and `OS release type: User`, then
  exited with its own reason: `unable to resolve 'mobile': 114 - The
  specified username does not exist`. iOS 16.7 ships NO opendirectoryd, NO
  DarwinDirectory framework and NO usermanagementd — the user resolver is
  **usermanagerd** (`/usr/libexec/usermanagerd`), which registers
  `com.apple.system.libinfo.muser`, the exact Mach service libinfo's muser
  module needs for getpwnam. Its plist was already in the mini root; the
  binary was not. Fix: add `/usr/libexec/usermanagerd` to the mini profile
  (+ the boot-closure verifier now asserts it).
* **The algo-9 corruption fix (found while chasing the same wall).**
  decmpfs algo 9 (uncompressed-inline) on APFS carries a 1-byte marker
  (0xCC) between the fpmc header and the raw bytes — payload length is
  `uncompressed_size + 1`. The old verbatim copy kept the marker and dropped
  the final byte, silently corrupting every algo-9 file in the image
  (135,724 files volume-wide; LaunchDaemons plists among them —
  usermanagerd's own plist would not have parsed). Verified on hosts,
  group, backboardd/configd/usermanagerd plists; unit test added; both the
  marker and marker-less forms decode verbatim now.
* **THE 'mobile' wall — corrupted symlink targets (run 40015518091, 2026-10-23).**
  With usermanagerd present the wall survived; the new msgbuf console-ring
  dump showed launchd exec'd at 30.5s, mapped the DSC (45x "not in trust
  cache" warnings the SWEET kernel tolerates), brought up lo0 — and died at
  31.45s, ~0.9s after exec, BEFORE any daemon could spawn (usermanagerd
  never had a chance; the muser lookup could not answer). The resolver
  chain falls through to the flat-file module (/etc/master.passwd — present
  in the image), which also failed: **put_file sliced the
  com.apple.fs.symlink xattr at v[2..], keeping the 2-byte length prefix
  from the 4-byte j_xattr_val header ([xflags u16][xlen u16]) — every HFS+
  symlink in the image carried the target "\x0c\x00private/etc" instead of
  "private/etc"**, so fopen("/etc/master.passwd") resolved to a garbage
  path. Fix: strip the full header (xattr_resolved; handles dstream-backed
  targets too). Regression test added. This also un-corrupts /var, /tmp
  and every other symlink the daemons use.
* **MILESTONE — launchd early boot COMPLETE; the xpcproxy wall (run 40018794158, 2026-10-23).**
  The symlink fix (907efd0) un-bricked the resolver: the real launchd exec'd
  at 30.58s, resolved `mobile` (the flat-file module finally read
  /etc/master.passwd through a clean /etc symlink), ran its real boot tasks
  (mount-phase/data-protection/restore-datapartition/init-with-data-volume/
  fips/keybag/usermanagerd/tzinit/prng_seedctl — the absent ones "Skipping
  boot-task" as designed), listened on TCP 49152 + 62078 (lockdownd's port —
  the real network stack driven by the real launchd), and at 33.15s printed
  its own milestone: `[Event: boot] Early boot complete. Continuing system
  boot.` — then kept running (no exit, no panic for the whole 240 s boot)
  while cycling the user/501 (mobile) domain: SpringBoard, tccd and friends
  in WILL_SPAWN/spawn-scheduled/throttle loops. Two observability facts
  learned: (a) the launchd os_log mirror writes straight to the fbcon
  WITHOUT entering the kernel msgbuf ring (the ring ends at
  "Early boot complete"; fb.txt and msgbuf.txt are complementary windows);
  (b) the kernel messages scroll off the last fb screen once launchd floods
  the console — the verdict must read BOTH. The next wall names itself in
  every spawn: `Could not spawn process /usr/libexec/xpcproxy: 2: No such
  file or directory` — launchd's spawn TRAMPOLINE (launchd -> xpcproxy ->
  exec target, both domains) is /usr/libexec-resident and the mini profile
  had excluded the directory. Fix: add `/usr/libexec/xpcproxy` to
  mini_keep_libexec (+ boot-closure verifier assertion); verdict v2 with
  launchd-positive stages (user-domain / launchd-early-boot / launchd-exec).
* **xpcproxy execs — the spawn machine runs (run 40021272762, 2026-10-23).**
  With the trampoline present the M3 verdict v2 PASSED at stage
  `user-domain`: launchd completed early boot again, 68 xpcproxy spawns
  waved through both domains, and the PRESENT daemons exec'd for real —
  mediaserverd, fairplayd.H2, bluetoothd, WirelessRadioManagerd (all
  /usr/sbin) running as processes with memorystatus priorities, zero
  kernel panics for the whole boot. xpcproxy turned into the wall
  inventory machine: `Missing executable detected. Job: '...'
  Executable: '/path'` — plus xpc's own `0x6f` init failures for the
  targets it can't set up. The first wave (launchd-spawn-map.md order)
  joins the mini root: backboardd (M5/M6 Mach-service gate owner),
  containermanagerd, usabilityd, keybagd, lockdownd, logd, configd,
  watchdogd, tccd. Observability: launchd's os_log mirror bypasses the
  kernel msgbuf and floods the last fb screen, so the CLI gained
  `--snap-every N` — interval framebuffer screenshots (fresh sequential
  QMP connections, guest keeps running) that screen-decode turns into
  the launchd log TIMELINE in artifacts.
* **The first wave lives — system domain STABLE (run 40023667468, 2026-10-23).**
  With the first wave in the root, the kernel msgbuf recorded the full
  exec cascade: distnoted, bluetoothd, logd, watchdogd, mediaserverd,
  notifyd, configd, keybagd, backboardd, containermanagerd, lockdownd,
  usermanagerd, fairplayd.H2, IOMFB_FDR_Loader, WirelessRadioManagerd —
  all by 39.1s, all through the real xpcproxy trampoline. keybagd made
  successful AppleKeyStore kernel calls (SWEET AKS sel 0 -> 0x0). The
  `--snap-every 20` timeline (fb-0020..fb-0200.txt) shows launchd's log
  settling at 39.6s: present daemons running (`Successfully spawned
  logd[69]`, bluetoothd[63] running), missing ones parked ("Service
  setup event to handle failure and will not launch until it fires") —
  200 s of steady state, no crash loops, no panics. xpcproxy's inventory
  named the next absent binaries: /usr/libexec/remoted, fseventsd,
  thermalmonitord (all added now). Next goal: the user domain's main job
  com.apple.SpringBoard (WILL_SPAWN-cycling on its binary since the
  xpcproxy run) — the app bundle joins the mini root and its os_log is
  mirrored to the console (CONSOLE_LABELS) so its startup names the
  next wall (M5/M6 services: com.apple.backboard.display.services,
  com.apple.iohideventsystem).
* **SpringBoard execs — the real shell is alive (run 40029781267, 2026-10-23).**
  With the app bundle present, the kernel msgbuf recorded
  `exec SpringBoard` at 31.88s: pid 5, **priority 160, 330 MB jetsam
  limit** — the real SpringBoard profile launchd assigns on hardware.
  The binary came straight from the IPSW's CoreServices (+7.5 MB md0),
  validated by xpcproxy (access X_OK) and exec'd against the DSC. The
  whole xpcproxy-named set joined: fseventsd, remoted, thermalmonitord,
  OTACrashCopier (running). **Zero exit events in the kernel ring** and
  no re-exec of SpringBoard for the rest of the boot: the shell process
  was alive and idle-blocked, not crash-looping. payload 3.9496 GB
  (bank headroom ~293 MB). Wall now: the Mach bootstrap service
  ecosystem — SpringBoard + backboardd alive but the
  display-services/HID negotiations produce no console traffic yet; the
  32-38 s startup-log window fell between the 20 s snapshots, so the
  next run closes the gap with --snap-every 5.
* **THE MACH WALL, measured — the M4/M5 device work is now the path (run 40034787036, 2026-10-23).**
  A 20-minute boot (38 snapshot windows) gave the definitive answer to
  "slow or stuck": the launchd log froze at 00:00:44 guest time and never
  moved again; the kernel idle-loops on timer IRQs at a fixed PC for the
  remaining 19 minutes; no daemon exits; nothing new logs. The whole
  userland is ALIVE and parked in mach_msg waits. The dependency chain
  (userspace-deps.md): backboardd cannot complete its launchd check-in
  until it registers `com.apple.backboard.display.services`, which needs
  a working **IOMobileFramebuffer user client**; SpringBoard waits for
  backboardd. The kernel side explains it: the QEMU ADT carries a
  SYNTHETIC `/arm-io/disp0` (`compatible = "sweet,display"`, canvas
  props only) that the SWEET-patched console driver matches — but the
  REAL `IOMobileFramebuffer` kext matches `disp0,t8015` with the D22AP
  register map (`disp0 @ 0x18000000`, `dart-disp0 @ 0x18704000`,
  `mipi-dsim-1 @ 0x18400000`), none of which exists on QEMU `virt`.
  No match → no user client → no registration → the whole graph waits.
  **This is the originally-planned M4/M5 wall** (DART + disp0 register
  model in QEMU).
  Nuance for the next session: the SWEET patch set already includes
  0046 "AppleCLCD/IOSurface/IOMobileFramebuffer on /arm-io/disp0" (the
  kernel console renders through AppleCLCD today), so IOMFB may already
  ATTACH on the synthetic node — the block is then inside the user
  client's hardware-dependent flow (surface/dart/interrupt ops that no
  QEMU device ever completes). The M5 plan's own alternative — "a
  virtual-display patch keyed on the IOMFB kext mapping the
  DART-translated surface to FB 0x9c000000" — remains the designed
  answer. Scoped experiments, cheapest first: (a) IOKit-verbose boot
  (debug=0x8) to capture IOMFB's attach/user-client state on the
  synthetic node; (b) gdb-attach at end-of-boot via QMP `gdbserver` HMP
  to read backboardd's thread state; (c) the virtual-display kernel
  patch (upstream SWEET patch set) or QEMU `unimplemented-device`
  stubs at the real D22AP MMIO bases (reads-as-zero; a half-matched
  real IOMFB may panic — revert restores the proven stable state).
* **Daemon-plist tweak verified applied; os_log does NOT route to stderr (run 40041071761, 2026-10-23).**
  The 270fdd8 diagnostics settled the invisibility question:
  `daemon plists: 491 seen, 0 shared-extent bypasses, 7 os_log-mirrored
  (com.apple.SpringBoard, com.apple.backboardd, com.apple.configd,
  com.apple.containermanagerd, com.apple.logd, com.apple.mediaserverd,
  com.apple.watchdogd), 108 parse failures: not a binary plist`. Three
  facts: (1) the per-daemon surgery WORKS and reaches the right files —
  the 108 XML plists (usermanagerd, tccd, notifyd among them) are
  skipped verbatim by the binary-only parser; (2) no hard-link bypass;
  (3) the daemons STILL log nothing to the console — so the earlier
  "tweak didn't land" reading was an artifact of the guest-strings GREP
  FILTER (OS_ACTIVITY_DT_MODE lines never matched any pattern and were
  filtered out before hitting the artifact). The real conclusion: on
  iOS 16.7, os_log does not honor OS_ACTIVITY_DT_MODE /
  CFLOG_FORCE_STDERR routing to stderr for these daemons — their logs
  go to logd (running since the first wave), which persists them to
  /var/db/diagnostics as tracev3 on the md0. The next visibility tool
  is a tracev3 reader over the md0 dump (the repo already parses
  binary plists / ADT / IM4P / decmpfs — same discipline), NOT more
  plist env surgery. The MACH wall itself is unchanged (M4/M5 IOMFB).
* **M4/M5 device models landed — the display/DART/MIPI QEMU fork exists, and three boot-configuration walls are measured (2026-10-24, sandbox session).**
  The "much richer QEMU device model" exists now: `qemu-sweet/` (QEMU
  10.0.0 + three new devices, built by `qemu-sweet/build.sh`, CI job
  `qemu-m5-display.yml`) adds `sweet-dart` (dart-disp0 @ 0x18704000 +
  remap @ 0x18700000, real `IOMMUMemoryRegion` — stage-1 bypass/pass-through
  with a real DMA-deny fault path), `sweet-mipi-dsim` (@ 0x18400000) and
  `sweet-disp0` (@ 0x18000000 + aux/pmgr windows) whose DMA address space is
  rooted AT the DART's IOMMU; a host-driven HMP hook
  (`sweet-disp0-swap <PA>`) blits a surface through the DART AS into the
  framebuffer at 0x9c000000. All writes trace into `qemu.log`
  (`-d unimp,guest_errors`). `make-adt --display` synthesizes the real
  D22AP display subtree (exact register maps from `devicetree-d22ap.md`;
  disp0 keeps `sweet,display` so patch 0046's console driver keeps
  matching). Measured in the sandbox (real kernel, stock and fork QEMU):
  * **debug=0x8 is NOT "IOKit verbose"** — it is DB_KDB: the kernel
    configures serial debugging and waits for a KDB connection forever
    (first-wall.md's plan line was never actually run; M2 used 0xc).
  * **No RAMDisk ⇒ no boot.** Without ANY `rd=` payload the kernel parks in
    `_cpu_idle` right after "configuring debugging over serial" and NEVER
    reaches IOKit output — M2's "empty initrd" was load-bearing. The M5
    pipeline now carries an empty cpio (TRAILER!!!-only) as md0.
  * **`interrupts` without `interrupt-parent` WEDGES the boot.** With the
    real display `interrupts` arrays in the synthetic nodes (no AIC to
    point at), nub creation hangs before ANY IOKit output (kernel parked in
    `_ml_get_timebase`, zero device MMIO). IOKit resolves orphan
    `interrupts` against the ROOT controller (our arm,gic-v3 node) —
    "inert without interrupt-parent" is FALSE. The subtree omits
    interrupts now; the boot with the full display subtree reaches the
    M2 wall (`cannot mount root, errno = 19` mount-retry loop) — IOKit
    matching survives the new nodes.
  * **The real drivers still do not probe the synthetic nubs** (zero
    register traffic on all three devices; `device_type` swap tested —
    no effect). The next wall is kernel-side: the SWEET patch set's
    IOKit-match tolerance (Class C #2) governs whether AppleT8015DART /
    AppleSynopsysMIPIDSI personalities ever start against the synthetic
    tree — re-enabling them (or a GIC-mapped interrupts + AIC-model step)
    is the follow-up, with the device models and traces ready.
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
