# ALL-LIBS PLAN — every library accounted for, until full iOS boots in QEMU

This is the completion-level plan: every library, binary and daemon the full
iOS 16 boot needs, classified into exactly three action classes, with the
build order and acceptance gates. Every number below comes from the real
iPhone X 16.7.16 IPSW (see provenance in `userspace-deps.md`,
`devicetree-d22ap.md`, `kc-kexts.md`, `launchdaemons.txt`).

---

## 1. The three action classes

| Class | Meaning | Count (real data) |
|---|---|---|
| **A — RUN FROM IPSW** | Closed-source Apple binaries; they need services, not building. Extracted/browsed by this repo's tooling, loaded as-is. | ~all userspace: **239 kernel kexts**, **491 launch daemons**, ~2,000+ framework binaries (inside the dyld shared cache), backboardd's **46 dylibs**, SpringBoard stub's **4 dylibs** |
| **B — BUILD (Rust, this repo)** | The QEMU-side world: platform devices the kernel probes + boot plumbing + control plane. Nothing Apple-published; everything else is public register docs. | the device list in §3 (~25 devices, 8 of them launch-critical) |
| **C — PATCH (kernel/boot args)** | Small, honest deltas to XNU's behavior so the real drivers accept virtual hardware. | patch classes in §4 |

Nothing is classified "reimplement": we never rewrite Apple code. The only
things we ever write are Class B (QEMU devices/plumbing) and Class C
(behavior deltas), both ours to write.

## 2. The full boot stack, layer by layer

| Layer | Components | Class | Status |
|---|---|---|---|
| L0 QEMU platform | `virt` machine, CPU, RAM, PL011, loader @ FB `0x9c000000`, shared-memory guest RAM | B | **done** |
| L1 Boot chain | shim (MMU) → payload (ADT + boot_args + kernel) — `make-adt`/`make-payload` | B | **done** |
| L2 XNU kernel | real `kernelcache.release.iphone10b` (FAT→arm64, 50.9 MB decoded) + 81 SWEET patches | A+C | **boots** (launchd reached, zero panic) |
| L3 IOKit platform | AIC, PMGR×2 (t8015+generic), DART, Samsung UART/SPI, GPIO, WDT, NCO, DWI, SMC, AOP, PMP, SART, AES — matched by real kexts | B (devices) + A (drivers) | **next** (M3) |
| L4 Storage/rootfs | md0 ramdisk today; ANS2 NVMe coprocessor + SART for full restore disk | A+B | ramdisk path works; ANS2 = M4 optional |
| L5 Display pipeline | `disp0,t8015` + `dart-disp0` + `mipi-dsim-1,synopsys` + `AppleSynopsysMIPIDSI` + IOMobileFramebuffer + IOSurface | B+A | **M5 core** |
| L6 Userspace services | launchd graph: 491 daemons; UI-critical subset in §5 | A (plists patched via ramdisk overlay only) | M6–M7 |
| L7 UI | backboardd → SpringBoard (stub → SpringBoard.framework in DSC) → home screen | A | **M7 gate** |

## 3. Class B: the QEMU device build list (launch-critical order)

Cross-matched from the real ADT (`devicetree-d22ap.md`) and the real kext
personalities (`kc-kexts.md`) — each row: device node → matching driver →
register reference:

| # | QEMU device | ADT node (base) | Matching kext personality | Ref | Gate |
|---|---|---|---|---|---|
| 1 | **AIC** interrupt controller | `arm-io/aic` `aic,1` @ `0x32100000` | `AppleAIC` (aic,1) | mainline Linux `apple,aic` + t8010 DTS | M3 |
| 2 | **PMGR** clocks/gates (always-on stubs first) | `arm-io/pmgr` `pmgr1,t8015` @ `0x32000000` | `AppleT8015PMGR`, `ApplePMGR` | Asahi PMGR docs, t8015 DTS | M3 |
| 3 | **Samsung UART** ×8 | `arm-io/uart0..7` `uart-1,samsung` @ `0x2e600000+` | `AppleSamsungSerial: uart-1,samsung` | pongoOS, linux s5l-uart | M3 |
| 4 | **WDT** | `arm-io/wdt` `wdt,t8015` @ `0x352b0000` | AppleWatchdog | t8010 DTS | M3 |
| 5 | **GPIO / pinctrl** ×3 | `arm-io/gpio` `gpio,t8015` @ `0x33100000` (+aop, nub) | AppleGPIO/pinctrl | t8010 DTS | M3 |
| 6 | **NCO** counters | `arm-io/nco` @ `0x32044000` | `AppleS5L8960XNCO` | t8015 DTS | M3 |
| 7 | **DWI** voltage/curr sensors | `arm-io/dwi` @ `0x32200000` | AppleDWI | t8010 DTS | M3 |
| 8 | **AOP/PMP/SMC coprocessors** | `arm-io/aop|pmp|smc` `iop*,t8015` | AppleARMPMUEpilogue, SMC clients | pongoOS rtbuddy model | M3–M4 (mailbox stubs) |
| 9 | **AES engine** | `arm-io/aes` `aes,s8000` @ `0x2e008000` | AppleAES | pongoOS | M4 |
| 10 | **DART** IOMMU ×6 (disp0/scaler/jpeg/isp…) | `arm-io/dart-*` `dart,t8015` | `AppleT8015DART: dart,t8015` | Asahi DART | **M4/M5** |
| 11 | **disp0 + MIPI-DSI** | `arm-io/disp0` @ `0x18000000`, `mipi-dsim` @ `0x18400000` | `AppleSynopsysMIPIDSI`, IOMobileFramebuffer | IOMFB reverse-eng (checkra1n-era docs) | **M5** |
| 12 | SPI/I2C stubs | `arm-io/spi1..2`, `i2c0..3` | `AppleSamsungSPI: spi-1,samsung`, `AppleS5L8940XI2C` | t8010 DTS | M5 (HID) |
| 13 | HID transport (touch) | `arm-io/spi1/multi-touch` `hid-transport,k1` | `AppleHIDTransportSPI: hid-transport,k1/spi` | — | **M6** |
| 14 | ANS2 + SART (NVMe) | `arm-io/ans` @ `0x57000000`, `sart-ans` @ `0x59c50000` | AppleANS2, AppleSART | Asahi NVMe (ans2) | optional (md0 suffices) |
| 15 | SEP / Secure Enclave | `arm-io/sep` `iop,t8015` @ `0x43000000` | AppleSEPKernel clients | — | **stub-only** (§6) |

Plus non-device Class B items already working: HTTP-range IPSW access, APFS
reader, framebuffer compositor, QMP/gdbstub/serial control plane.

## 4. Class C: kernel/boot patch classes (on top of the 81 SWEET patches)

1. **ADT adjustment** — synthesize/extend the ADT so every node in §3 points
   at emulated MMIO (make-adt already supports `--product` from the real
   D22AP tree; extend with device nodes). *Pure data, no code patch.*
2. **IOKit match tolerance** — where a driver insists on hardware behavior we
   don't model (PMGR gate polling, AIC v2 quirks), patch the driver's probe
   path in the kernelcache (SWEET-style) rather than fake the hardware.
3. **AMFI/codesign policy** — rootfs binaries must pass launch; verify the
   existing SWEET AMFI patches cover userspace CS enforcement (per-binary).
4. **SEP-absence tolerance** — daemons gated on SEP fail; ensure they fail
   *late* (after launchd check-in) so the boot graph continues.
5. **launchd graph** — the ramdisk overlay carries edited
   `/System/Library/LaunchDaemons/*.plist` (disable hardware-dead daemons,
   keep UI path). *Data, not code.*

## 5. Class A: the 491 daemons — triage rule

`launchdaemons.txt` = the real list. Triage:
* **launch-critical for UI** (must check in): `com.apple.backboardd` (already
  extracted: MachServices `com.apple.backboard.display.services` +
  `com.apple.iohideventsystem` = M5/M6 gates), `usermanagerd`,
  `usabilityd`, `lockdownd` (best-effort), `mobileassetd` (disable-ok),
  `SpringBoard` (LaunchAgents, M7), `iosched`, `configd`, `mDNSResponder`
  (net ok), `mediaserverd` (audio, tolerate-late).
* **hardware-dead** (disable in overlay): BTServer*, baseband/*,
  WiFiKeyStore, Apricot*, biometrics, hidf (real touch), compass, etc.
* **tolerate-fail**: everything else — launchd `KeepAlive` daemons that fail
  must not block the graph (patch = plist RunAtLoad off, data-only).

## 6. Honest parity boundary

What full parity **cannot** include on QEMU: SEP-rooted hardware crypto
(FaceID/TouchID enrollment, secure keychain(HW), Secure Enclave UI), baseband
(cellular), and hardware-unique UID keys. Everything else — kernel, drivers,
userspace daemons, SpringBoard rendering, app launch — is in scope of the
milestones above, and every rendered pixel will be labeled real-vs-host.

## 7. Execution order and gates (repeats RESEARCH-SPRINGBOARD.md §3)

M1 inventory (**done this commit**) → M2 first-wall log → M3 platform wave 1
(devices 1–7) → M4 DART+AES (10, 9) → M5 display (11, 12) → M6 HID input
(13) → M7 SpringBoard graph (§5 triage). Each gate = acceptance criterion in
the roadmap doc; each gate lands as its own commit + CI job.
