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

### W2 — make the real drivers probe the synthetic nubs (device traffic > 0)

Today `AppleT8015DART`, `AppleSynopsysMIPIDSIController`,
`IOMobileFramebuffer` never touch the models (`any_device_traffic=false`).
Options, in the order we should try them:

1. **SWEET patch-set personality work** (the M5 commit's declared next
   step): inspect why the personalities don't match the synthetic nubs
   (`IONameMatch` vs our `compatible` strings, `IOProviderClass`
   `AppleARMIODevice` vs our node names, absence of expected properties —
   `io-device-name`, `AAPL,phandle` conventions). Adjust `mkadt.rs` first
   (free), then the SWEET patch set if the kext demands more.
2. **GIC-mapped interrupts + AIC model**: if matching needs interrupts
   (M5 measured: dangling `interrupts` wedge nub creation — so this means
   a REAL AIC stand-in routed to the GIC, plus `interrupt-parent` in the
   subtree). Non-trivial; only if (1) proves interrupts are the blocker.
3. **`kexts` triage**: `kc-kexts.md` already maps personalities ↔ kexts;
   for each of the three drivers, record the exact mismatch reason from an
   IOKit verbose boot (`debug=0x14e` IOKit trace flags, serial mirrored)
   BEFORE patching anything.

**Gate:** `dart_register_writes > 0` in the m5-verdict (the CI already
collects it); the traces become the stage-2 register spec.

### W3 — DART stage-2: the real PTE walker (device model work)

Driven by the W2 traces, never by guesses (the stage-1 `qemu.log` traces
ARE the spec — see `qemu-sweet/README.md`):

* `sweet-dart`: TTBR[SID] → 4-level walk of guest RAM (page size 0x1000,
  `stt-idx-width=10` sub-TTBRs), real fault reporting, TLB invalidate ops.
* `sweet-disp0`: swap path keyed on the register the real IOMFB writes
  (replaces the host-driven HMP `sweet-disp0-swap` hook).
* `sweet-mipi-dsim`: DSI command FIFO acks so the `lcd,summit` panel
  interface completes.

**Gate:** a guest-programmed PTE translates a DMA to the right host page
(read-back of a known pattern through the DART AS); IOMFB-initiated FB
flip changes `fb.png` without host help.

### W4 — the IOMFB user client (userland × device)

M3 measured the MACH wall: userland alive, parked on a working
`IOMobileFramebuffer` user client. With W2/W3 the client has a device to
talk to. Measure the exact call that parks (`userspace-deps.md` gates),
extend the model only for what the call path touches.

**Gate:** the parking daemon gets past the IOMFB connect; the spawn cycle
advances one measured daemon further.

### W5 — SpringBoard first frame (the finish line of the plan, §M7)

BackBoard + HID injection (`com.apple.iohideventsystem`) and the first
composed frame on the emulated display. Everything upstream of this is
either already proven (M3 user-domain spawns, SpringBoard in the mini
root) or is W1–W4.

## 2. Immediate next actions (ordered)

1. **W1** — `--sweet-devices`/`--display-adt` in `ipsw-boot`; CI job that
   runs the unified pipeline and asserts the M3 verdicts with devices on.
2. **W2 diagnosis** — IOKit-verbose boot of the unified pipeline; record
   the per-driver match failure verbatim into `kc-kexts.md`; then patch
   `mkadt.rs` (cheap) → SWEET patch set (if needed).
3. **W3 stage-2 DART** — only after W2 traffic exists (the walker is
   written against measured writes, not imagined ones).

## 3. Honesty rules (unchanged)

* A missed gate produces a documented wall, never a fabricated success
  (§4 Risks of `RESEARCH-SPRINGBOARD.md`).
* Every claim in this file links to the run that measured it.
* The vendored QEMU tree is never edited in place: all changes are
  `overlay/` files or the wiring patch, so the vendor boundary stays
  auditable (`diff -rq` against the pristine tag must stay empty after a
  `build.sh` reset).
