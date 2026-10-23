# M2 — the first wall, measured (real kernel, real QEMU, 2026-10-23)

**Setup:** `kernel.development.sweet` (0071-class — the exact build that ran
the iOS GUI on the phone: strings `AppleCLCD` x31, `SweetHIDTouch` x30,
`pid_hibernate`, `SWEET AMFI` present) + ADT built by our `make-adt --qemu
--product <real D22AP tree>` + empty initrd + boot args
`debug=0xc serial=3 cpus=1 keepsyms=1 rd=md0 -noprogress wdt=-1`, QEMU 10.0.13
`-M virt` cortex-a76, 90 s.

**Result (fb.png console, evidence in this directory):**

```
BSD root: md0, major 1, minor 0
vnode_create_internal: alias vnode 0xfffffe24cdcdc370 is in state of change (start)
vnode_create_internal: alias vnode 0xfffffe24cdcdc370 is in state of change (end)
hfs_ValidateHFSPlusVolumeHeader: unknown Volume Signature : 0
hfs_mount: hfs_mountfs returned error=22 for device unknown-dev
cannot mount root, errno = 19
mount(N) failed            <- N = 5..13+, retried for the whole run
```

`qemu.log` independently shows the kernel alive the whole time: EL1 IRQ
exception take/return at varying kernel PCs (idle + timer), zero panic.

## What this proves

1. **IOKit startup is NOT the wall.** The SWEET patch set (81 patches,
   incl. 0046 AppleCLCD/IOSurface/IOMobileFramebuffer on `/arm-io/disp0`,
   0050 Sandbox policy, 0058 HID, 0062 RPMh/battery, 0063 touch) carries the
   kernel through IOKit matching panic-free on QEMU `virt` with our ADT.
2. **The wall is the rootfs content.** The kernel mounts `md0` (the RAM disk)
   as HFS+; our empty initrd has no HFS+ signature, so `mount` retries
   forever. Userspace never starts because there is no root volume.
3. **The console is fbcon, not UART**: `serial.txt` stays empty while the
   kernel console renders to the framebuffer (`pe_serial.c` 2x white-on-black
   glyphs at 1080x2400) — the pipeline's framebuffer IS the console; decode
   glyphs from `fb.png` (qemu-sweet.py `decode_screen` parity).

## The next milestone (unchanged plan, sharpened)

Provide `md0` with the **real iOS root** (the upstream project built phone
images v9-v14 from this IPSW and reached SpringBoard "Hello" + touch with
this kernel class). QEMU parity requires the same HFS+ root as a RAM disk:

* assemble the real rootfs from the IPSW (we already browse it: 318,071
  entries via HTTP ranges; upstream `mkiosroot.sh` + `sweet-root-tweaks.py`
  document the plist surgery: SetupDone, data_ark activation, DataMigration
  `LastSystemVersion`, `BKNoWatchdogs`, CommCenter removal, MediaToolbox
  `StartRecorderServer=false`),
* HFS+ format (`mkfs.hfsplus`), size ~5 GiB, guest RAM must cover it
  (`make-adt --highmem` twin + `-m` sizing),
* boot args move to the upstream set (`ignition_level=1
  unentitled_ios_sim_launch=1 launchd_unsecure_cache=1 rootrw=1 ufsrw=1 ...`).

Then the M2 wall text changes from `mount(N) failed` to real `launchd`
output — the first honest gate toward SpringBoard-in-QEMU.

## Evidence files (this directory)

* `fb-wall.png` — the fbcon console at end of run (mount retry loop)
* `qemu.log` — QEMU exception log (kernel alive, EL1 IRQs, no panic)
* `boot-verdict.json` — pipeline verdict (logo_visible=true, panic=false)
