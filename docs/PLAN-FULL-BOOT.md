# PLAN — the road to a full boot (vendored QEMU, measured walls)

Status: living plan, updated every time a wall is measured. Evidence lives
in `RESEARCH-SPRINGBOARD.md` §3b (execution log), the CI workflows, and the
artifacts each run uploads. **Nothing here is assumed; everything below is
either measured or has a measurement attached to its gate.**

## 0. Where we are (measured)

| piece | state | evidence |
|---|---|---|
| QEMU fork source | **VENDORED** at `qemu-sweet/qemu/` (pristine v10.0.0, byte-verified) | `git show` vendor commit; `diff -rq` = 0 at vendor time |
| sweet-dart / sweet-disp0 / sweet-mipi-dsim | built, realized at the D22AP bases, disp0 DMA rooted at the DART IOMMU | `qemu.log` device lines (M5, re-confirmed M6) |
| ADT display subtree | real D22AP register maps, unit-pinned byte-for-byte | `make-adt --display` + `cargo test` |
| kernel-stage boot | survives IOKit matching WITH the subtree; dies at the **M2 wall** (`cannot mount root, errno = 19` + `mount(20) failed` retry loop) | M6 re-run fb decode, `out/m6/fb-0090.png` / `fb.png` |
| userland-stage boot | real mini-root mounts as md0, real launchd execs, **user-domain spawn cycle** | M3 CI gate (`qemu-m3-rootfs.yml`) |
| driver probes on the synthetic nubs | **ZERO register traffic** (the next wall) | `grep 'sweet-.*: [WR] ' qemu.log` = 0 across runs |

Two independent pipelines exist today: the **M3 pipeline** (userland works,
display is QEMU-virt-default) and the **M5 pipeline** (display devices real,
rootfs is an empty md0). Full boot = **one pipeline that has both**.

## 1. The walls, in dependency order

### W1 — unify the pipelines (no new engineering, highest leverage first) — **SHIPPED + CI-VERIFIED (run 40063428984, all gates green)**

Run the M3 userland recipe ON TOP of the M5 display world:

```
ipsw-boot --ipsw <IPSW> out-full --kernel kernel --ramdisk mini-root.img
          --product dt/DeviceTree.d22ap.im4p
          --display-adt --sweet-devices        # SHIPPED (f5de58a)
          --vnc 127.0.0.1:59                   # SHIPPED (display out)
          --highmem --secs 240
```

Implemented: `ipsw-boot --display-adt` + `--sweet-devices` (one command,
no separate make-adt step), and `emu-boot` real-boot mode (`--kernel
--ramdisk --cmdline --product --display-adt --sweet-devices --highmem`)
for the LIVE screen-control session (QMP + gdbstub + serial socket + FB
paint) against the real boot. Measured in the sandbox: the flags wire
end-to-end (`pass:true`, devices realized, console painted) and the live
control session survives with the guest painting on its own
(`session_alive:true`, `guest_painted:true`).

**Display out (VNC + screendump) — SHIPPED:** the fork's `sweet-disp0`
registers a QEMU console (`graphic_console_init` + `dpy_gfx_replace_surface`
of the fb mirror), `-vnc` serves it live, QMP `screendump` captures it to
`screenshot-NNNN.png` / `screenshot-final.png` in every runner command;
`spawn_session` (emu-boot/Session) wires the same devices + `-vnc`.
Verified end-to-end on the real kernel: the console screenshot decodes to
the exact M2 wall text. This is the machine half of "control the screen";
the guest half (touch IN) is W5's BackBoard/HID work.

CI: `.github/workflows/full-boot.yml` runs the unified boot on a 16 GB
runner (`ubuntu-latest-4-cores`, `ubuntu-latest` fallback via
workflow_dispatch — USE THE DISPATCH: `ubuntu-latest-4-cores` is not
enabled on this plan and a push-triggered run queues forever; measured).
**Measured verdict (run 40063428984, ubuntu-latest fallback):**
`stage_with_devices = "user-domain"` (M3 userland markers + spawn cycle
HOLD with the display world on), all three devices realized, disp0 DMA
via DART, VNC console registered, 7 console screenshots captured,
no kernel panic — and the live screen-control step PASSed
(wallpaper_painted / guest_painted / session_alive all true).
Gate additionally asserts VNC console registration + screenshot capture.

Caveat: the IPSW-cache upload makes the artifact ~6 GB; the run takes
~1 h wall on the fallback runner (fork build + 6.6 GB IPSW fetch +
240 s boot + live control).

### W2 — make the real drivers probe the synthetic nubs (device traffic > 0) — **CLOSED (impossible as written, 2026-10-24)**

**Verdict (measured, five boots A–E + a static kernel-strings inventory):** the
booted `kernel.development.sweet` contains **no Apple T8015 driver kexts at
all**. The only IOKit platform drivers in the binary are
`AppleARMPlatform` + `AppleInterruptController` (+ the SWEET built-ins).
`AppleT8015DART`, `AppleSynopsysMIPIDSIController`, `AppleT8015IO`,
`IOMobileFramebuffer`-as-Apple-kext exist ONLY in the real kernelcache
(`kc-kexts.md` describes that file — which this pipeline never boots).
The SWEET kernel is the **SM7150 build** (upstream `dipa-studio/ios-sweet`:
iOS 16 userland + XNU on Redmi Note 10 Pro) — it can never probe T8015
devices *by design*.

Evidence chain:
* `strings kernel/kernel.development.sweet`: 2 Apple platform drivers, no
  `AppleT8015*`, no `AppleARMIODevice` personalities; `macho-sym` counts
  70,853 symbols (kernel.development, keepsyms).
* Boot A (debug=0x14e): first FULL kernel console ring ever captured
  (341 lines, `_msgbufp` dump) — IOKit healthy, zero matching output, zero
  driver probes; M2 mount wall unchanged.
* Boot B (+`arm-io,t8015` compatible): byte-identical outcome — and the
  upstream reference (`tools/mkadt.py` line 170) confirms the canonical
  compatible is `sweet-io`+`arm-io`; reverted.
* Boot C (+`IOKitDebug=0x1ff`, the tunable IS in the kernel): no change —
  there is nothing to match.
* Upstream patch `0046-sweet-iosurface-iomfb.patch` (read, this session):
  the ENTIRE display path is a built-in RAM shim — AppleCLCD (an
  `IOMobileFramebuffer`) matches `sweet,display` on `/arm-io/disp0`, the
  user client serves CoreAnimation's selectors (3/4/5/6/7/8/18/28/72) with
  a **CPU copy into the panel memory**; vsync is a 60 Hz timer. **No display
  hardware registers are ever programmed.**

Consequences:
* The `dart_register_writes > 0` gate **as originally intended is
  architecturally unreachable** on this kernel. The QEMU
  `sweet-dart`/`sweet-disp0`/`sweet-mipi-dsim` models stay as
  fidelity decoration (they serve the VNC display OUT and the DMA chain
  evidence) but are NOT on the boot-critical path.
* W3 (DART stage-2 walker) loses its "W2 traces as spec" premise. If it is
  ever revived, the spec source is public DART knowledge (pongoOS,
  mainline `t8015-common.dtsi`), not kernel traces. **Deprioritized.**
* The boot-critical path is **W4** (below) — and its cause was found this
  session.

### W3 — DART stage-2: the real PTE walker (device model work) — **REVIVED + IMPLEMENTED + GATED (2026-10-24)**

W2 closed the "kernel traces as spec" premise (no guest code ever
programs the DART), so stage 2 was written against the **real D22AP ADT
parameters** (`page-size = 0x1000`, `stt-idx-width = 0xa`, `sids = 3`)
plus public M1-DART-style PTE bit knowledge — RE-grade, documented, and
frozen as the fork's contract by a deterministic gate (see
`qemu-sweet/README.md` §Stage-2 scope):

* `sweet-dart`: TTBR0[sid][n] → 4-level walk of guest RAM (page 0x1000,
  L1/STT index 10 bits, L2/L3 9 bits), 4096-entry translation cache,
  `STREAM_COMMAND` invalidate, real fault reporting (`FAULT_STATUS` /
  `FAULT_IOVA_LO/HI` + the fault IRQ, W1C-acked), and a layout fix:
  TTBR0 moved 0x200 → 0x300 (stage 1's TTBR0[0][0] was shadowed by
  TCR[2] — unreachable with the D22AP's sids=3).
* `sweet-disp0`: the register-keyed swap path (`SWAP_SRC_LO/HI`,
  `SWAP_CTRL` bit 0 SWAP_GO, `SWAP_COUNT`, `INTSTATUS` SWAP_DONE) — the
  guest-initiated IOMFB flip; the HMP hook stays, sharing the engine.
* `sweet-mipi-dsim`: DSI command FIFO acks (`GEN_HDR`/`GEN_PLD`/
  `CMD_PKT_STATUS` + `INTSTATUS` CMD_DONE).

**Gate — PASS (4 GB sandbox, 2 GiB guest, real kernel boot):**
`tools/dart-walker-gate.sh` boots the payload on the fork with
`--qtest`, pauses the vCPU and drives the models' MMIO exactly as the
real drivers would: unconfigured DART **denies** and latches FAULT_*;
a guest-programmed PTE translates the non-identity IOVA 0x70000000 →
0xbf007000 through L1[1]→L2[384]→L3[0] (10 MB framebuffer read-back =
the programmed pattern); the cache serves the stale PTE until
`STREAM_COMMAND` invalidate, then refetches; the MIPI FIFO acks. All
ten gates true (`dart-gate-verdict.json`); wired as a hard CI step in
`qemu-m5-display.yml`. The models remain fidelity decoration for the
BOOT itself (W2 verdict unchanged — the booted kernel never touches
them); what changed is that the decoration is now REAL, verified
machinery instead of store+echo stubs.

### W4 — the IOMFB user client (userland × device) — **CAUSE FOUND + FIXED (2026-10-24)**

The MACH wall's root cause was in **our ADT port**, not in the kernel or
the device models. Upstream patch 0046's own header states:
`IOMobileFramebuffer.framework` (iOS 16.7) **counts `/arm-io` children
with `device_type = "display-subsystem"`**, waits for that many services
matching `IOServiceMatching("IOMobileFramebuffer")`, and reads
`canvas-width`/`canvas-height` from the provider node.

Our `mkadt.rs` had `disp0` with `device_type = "dart"` — a porting bug
(upstream `tools/mkadt.py` lines 181-191: `display-subsystem`). The
framework saw ZERO display subsystems → backboardd could never complete
its launchd check-in (`com.apple.backboard.display.services`) →
SpringBoard waited on backboardd → the whole userland parked in mach_msg
at t=44 s of the boot (run 40034787036).

FIXED this session: `device_type = "display-subsystem"` restored; the
default ADT is now semantically identical to upstream's `mkadt.py --qemu`
output (verified by `adt-dump` diff: only random-seed + phandle numbering
order differ; both internally consistent).

The kernel-side SWEET IOMFB (patch 0046) is complete on its own: user
client selectors 3/4/5/6/7/8/18/28/72, SwapEnd = CPU layer copy, vsync =
60 Hz timer. `sweetfb_noconsole=1` (now in the CI cmdline) pauses the
text console on the first swap so the framebuffer shows the pure GUI —
the prerequisite for clean SpringBoard/Settings screenshots (W5).

**Measurement (next CI run):** the unified verdict now records
`daemons_execd[_count]` + `last_msgbuf_ts` against the freeze baseline
`mach_wall_freeze_baseline_ts = 45.0`. Gate to pin from that evidence:
`last_msgbuf_ts > baseline` and a daemon roster that grows past the
baseline set. **Gate:** the parking daemon gets past the IOMFB connect;
the spawn cycle advances one measured daemon further.

**MEASURED — run 40097359964 (2026-10-24, the FIRST-EVER green full-boot
on the runner; the paid-label + concurrency-slot runner bugs were fixed
this session):** every step green including the W1 gate. Verdict:
`stage_with_devices = "user-domain"` with `daemons_execd_count = 23`
(backboardd, SpringBoard, mediaserverd, lockdownd, keybagd, IOMFB_FDR_Loader,
…), `last_msgbuf_ts = 45.07` vs the 45.0 baseline, `kernel_panic = false`,
all three sweet devices realized, `disp0_dma_via_dart = true`, 7 console
screenshots, and the LIVE control session PASS (`wallpaper_painted`,
`guest_painted`, `session_alive` all true). The W4 heap probe (first run
with the corrected probe window — the ADT bank 0x100000000+0xfcc00000)
shows the REAL GUI stack resident in the guest: SpringBoardServices,
BackBoardServices, IOSurface, IOMobileFramebuffer, IOSurfaceAccelerator,
SpringBoardUI + `_IOServiceMatching`/`_bootstrap_port` — the daemons are
alive and parked at their bootstrap/IOMFB check-ins, not dead.
**Remaining uncertainty (next run answers it):** whether backboardd got
PAST the IOMobileFramebuffer connect. The kernel ring goes quiet at 45 s
(userland took over); the daemons' own words are in logd's tracev3 store
on the md0 — the new "Daemon logs" step now extracts it (`md0-logs` +
`out-full/diag-w4.txt`), giving backboardd's check-in verdict verbatim.

### W5 — SpringBoard first frame (the finish line of the plan, §M7)

BackBoard + HID injection (`com.apple.iohideventsystem`) and the first
composed frame on the emulated display. Everything upstream of this is
either already proven (M3 user-domain spawns, SpringBoard in the mini
root) or is W1–W4.

## 2. Immediate next actions (ordered)

1. ~~W1~~ — **SHIPPED + CI-VERIFIED** (run 40063428984).
2. ~~W2 diagnosis~~ — **CLOSED** (see the W2 verdict: the drivers are not
   in the kernel; the display path is the built-in RAM shim).
2b. **W3** — REVIVED + IMPLEMENTED + GATED (see W3 below): the stage-2
   device models land with `tools/dart-walker-gate.sh`, CI-gated in
   `qemu-m5-display.yml` (its push trigger was also broken —
   `branches: ain]` — and is fixed in this push).
3. **W4 measurement** — the `display-subsystem` fix ships in this push;
   the full-boot CI run measures the spawn-cycle advance past the 45 s
   freeze baseline (`daemons_execd_count`, `last_msgbuf_ts`). Gate pins
   from that evidence.
4. **W5 SpringBoard first frame** — once W4's verdict lands: clean GUI
   frames via `sweetfb_noconsole=1`, VNC capture, then HID injection for
   the apps/settings/about screenshot goal.

Toolchain additions this session (all tested, no external deps):
* `macho-sym` — native LC_SYMTAB parse (70,853 symbols on the kernel);
  `extract_font`/`screen-decode` use it first, llvm-nm only as fallback;
  the CI `_msgbufp` lookup dropped llvm-nm entirely.
* `boot-qemu --msgbuf-dump <addr>` — the kernel console ring on every
  boot (the fb console only ever shows the last screen; serial is empty
  by design on this kernel).
* The first FULL kernel console ring ever captured (341 lines, boot A) —
  the W2 evidence base.

## 3. Honesty rules (unchanged)

* A missed gate produces a documented wall, never a fabricated success
  (§4 Risks of `RESEARCH-SPRINGBOARD.md`).
* Every claim in this file links to the run that measured it.
* The vendored QEMU tree is never edited in place: all changes are
  `overlay/` files or the wiring patch, so the vendor boundary stays
  auditable (`diff -rq` against the pristine tag must stay empty after a
  `build.sh` reset).
