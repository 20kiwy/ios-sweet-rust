//! Build the minimal Apple Device Tree the SWEET xnu needs to boot.
//! Port of `tools/mkadt.py` (constant for constant, property for property).

use crate::adt::{self, Node};
use crate::{bail, Result};

/// Kernel-managed RAM: above every Qualcomm XPU carve-out (see NOTES.md)
pub const PHYS_BASE: u64 = 0xb000_0000;
pub const MEM_SIZE: u64 = 0x4d00_0000;
/// Panic log carved from the TOP of the dram range, exposed as /pram +
/// /chosen/embedded-panic-log-size. Without it the kernel hangs right after
/// "kdp_core zlib memory" (reproduced on QEMU and on ios-fajita).
pub const PANIC_LOG_SIZE: u64 = 0x100000;
/// --highmem (phone only): the whole upper DDR bank.
pub const HIGH_PHYS_BASE: u64 = 0x1_0000_0000;
/// The proven highmem bank (the only configuration where xnu reached
/// launchd and dyld mapped the shared cache successfully). It is SMALL on
/// purpose: >=4GiB crosses xnu's ncl_table row and every
/// bsd_scale_setup-derived limit changes with it — measured failures at
/// 4GiB: mbinit VERIFY panic (uipc_mbuf.c:1743) unpinned, and with
/// ncl=49152 pinned the boot reaches launchd but dyld then fails
/// "Library not loaded: /usr/lib/libSystem.B.dylib". At 0xff000000 with
/// the big root, arm_init died in current_percpu_base (NULL per-cpu base).
/// The bank top must therefore bound the ramdisk: keep the mini root
/// (with the dyld shared cache) under ~3.9GiB — see rootfs.rs mini_keep.
pub const HIGH_MEM_SIZE: u64 = 0xfcc0_0000;
/// /arm-io children are relative to this; must not be 0 (serial_init bails).
pub const SOC_BASE: u64 = 0x0100_0000;

pub const GICD: u64 = 0x17a0_0000;
pub const GICD_SIZE: u64 = 0x10000;
pub const GICR: u64 = 0x17a6_0000;
pub const GICR_SIZE: u64 = 0x100000;
pub const FB: u64 = 0x9c00_0000;
pub const FB_SIZE: u64 = 0x1700_000;
pub const SWEET_ECID: u64 = 0x5357_4545_5400_0001; // 'SWEET' + 1, synthetic
pub const FB_W: u32 = 1080;
pub const FB_H: u32 = 2400;
pub const FB_STRIDE: u32 = 1080 * 4;
/// Internal UFS host (ufshc@1d84000 "ufs_mem"); QEMU virt has none.
pub const UFS: u64 = 0x01d8_4000;
pub const UFS_SIZE: u64 = 0x3000;
pub const TIMEBASE: u32 = 19_200_000;
pub const VTIMER_INTID: u32 = 19;
/// QEMU virt (-M virt,gic-version=3 -cpu cortex-a76).
pub const QEMU_GICD: u64 = 0x0800_0000;
pub const QEMU_GICR: u64 = 0x080a_0000;
pub const QEMU_TIMEBASE: u32 = 62_500_000;
pub const QEMU_VTIMER_INTID: u32 = 27;

/// iBoot's SHA-384 of the boot manifest; /sbin/mount -P 1 exits without it.
/// Fixed stand-in value (parity: sha384(b'sweet boot manifest')).
pub fn boot_manifest_hash() -> [u8; 48] {
    sha384(b"sweet boot manifest")
}

pub struct AdtOptions<'a> {
    pub qemu: bool,
    pub highmem: bool,
    /// Parsed /product node of the real D22AP tree (MobileGestalt identity).
    pub product: Option<&'a [(String, Vec<u8>)]>,
    /// 64-byte /chosen/random-seed (None = fixed SWEET seed for reproducibility).
    pub seed: Option<Vec<u8>>,
    /// (phys_base, mem_size) override for low-RAM QEMU hosts: the demo kernel
    /// reads everything from boot_args, so a 2 GiB window above QEMU virt's
    /// RAM base works there (the real xnu expects the phone layout).
    pub phys_override: Option<(u64, u64)>,
    /// Add the REAL D22AP display subtree (M4/M5): /arm-io/disp0 gains the
    /// `disp0,t8015` compatible + register map, and /arm-io/dart-disp0 +
    /// /arm-io/mipi-dsim appear with their real D22AP properties. The
    /// `sweet,display` compatible and the canvas props stay on disp0 so the
    /// SWEET-patched console driver (AppleCLCD, patch 0046) keeps matching.
    /// The register maps come from the real DeviceTree dump
    /// (docs/research/devicetree-d22ap.md, lines disp0/dart-disp0/mipi-dsim).
    pub display: bool,
}

impl Default for AdtOptions<'_> {
    fn default() -> Self {
        AdtOptions {
            qemu: false,
            highmem: false,
            product: None,
            seed: None,
            phys_override: None,
            display: false,
        }
    }
}

/// Low-RAM preset: phys 0x48000000, 0x70000000 bytes -> QEMU -m 2G.
pub const LOW_PHYS_BASE: u64 = 0x4800_0000;
pub const LOW_MEM_SIZE: u64 = 0x7000_0000;

/// /product properties of the real D22AP tree, without the syscfg placeholders
/// (flag bit set) and the node bookkeeping. Port of `mkadt.d22ap_product()`.
pub fn d22ap_product(buf: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let (root, _) = adt::parse(buf)?;
    let product = root
        .children
        .iter()
        .find(|c| c.prop("name") == Some(b"product\0".as_slice()))
        .ok_or_else(|| crate::Error::Msg("no /product node".into()))?;
    Ok(product
        .props
        .iter()
        .filter(|(k, f, _)| *f == 0 && k != "name" && k != "AAPL,phandle")
        .map(|(k, _, v)| (k.clone(), v.clone()))
        .collect())
}

fn u32v(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn u64v(v: &[u64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn node(name: &str, props: Vec<(&str, Vec<u8>)>, children: Vec<Node>) -> Node {
    let mut n = Node::new(name);
    for (k, v) in props {
        n.props.push((k.to_string(), 0, v));
    }
    n.children = children;
    n
}

pub fn build(opts: &AdtOptions) -> Result<Node> {
    let (mut phys_base, mut mem_size) =
        if opts.highmem { (HIGH_PHYS_BASE, HIGH_MEM_SIZE) } else { (PHYS_BASE, MEM_SIZE) };
    if let Some((pb, ms)) = opts.phys_override {
        phys_base = pb;
        mem_size = ms;
    }
    // Carve the panic log off the top so the kernel never allocates over it.
    let panic_base = phys_base + mem_size - PANIC_LOG_SIZE;
    mem_size -= PANIC_LOG_SIZE;
    let (gicd, gicr, timebase) = if opts.qemu {
        (QEMU_GICD, QEMU_GICR, QEMU_TIMEBASE)
    } else {
        (GICD, GICR, TIMEBASE)
    };
    let seed = opts.seed.clone().unwrap_or_else(|| {
        // Fixed synthetic seed: QEMU boots are reproducible; the phone build
        // passes a random seed via CLI (--seed).
        let mut s = vec![0u8; 64];
        for (i, b) in s.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(0x5a);
        }
        s
    });
    let mut phandle = 1u32..1000;
    macro_rules! ph {
        () => {
            ("AAPL,phandle", u32v(&[phandle.next().unwrap()]))
        };
    }

    let chosen = node(
        "chosen",
        vec![
            ph!(),
            ("dram-base", u64v(&[phys_base])),
            ("dram-size", u64v(&[mem_size])),
            ("random-seed", seed),
            ("debug-enabled", u32v(&[1])),
            ("embedded-panic-log-size", u32v(&[PANIC_LOG_SIZE as u32])),
            ("firmware-version", adt::cstr("sweet-shim")),
            ("system-firmware-version", adt::cstr("sweet-shim")),
            ("boot-manifest-hash", boot_manifest_hash().to_vec()),
            ("crypto-hash-method", adt::cstr("sha2-384")),
            // MobileGestalt's screen class; 1080x2400 @3x = 360x800 points.
            ("display-scale", u32v(&[3])),
            // XNU derives IOPlatformUUID (gethostuuid) from these on arm.
            ("unique-chip-id", (SWEET_ECID).to_le_bytes().to_vec()),
            // Chip-id of the A11 that the iPhone X userland expects.
            ("chip-id", u32v(&[0x8015])),
            // Security status as on a dev-fused, undemoted board (0,0,1,1):
            // mobileactivationd then takes its native Hactivation branch.
            ("certificate-production-status", u32v(&[0])),
            ("effective-production-status-ap", u32v(&[0])),
            ("certificate-security-mode", u32v(&[1])),
            ("effective-security-mode-sep", u32v(&[1])),
        ],
        vec![
            node("memory-map", vec![ph!()], vec![]),
            // Local boot policy (kern_csr.c): SIP relaxed like `csrutil
            // disable` + unauthenticated (unsealed) root. The iOS shared cache
            // is in no trust cache, so dyld can only map it with
            // CSR_ALLOW_UNRESTRICTED_FS.
            node(
                "asmb",
                vec![ph!(), ("lp-sip0", u64v(&[0x6f])), ("lp-sip1", u32v(&[1]))],
                vec![],
            ),
        ],
    );

    // SWEET SMP: all eight cores in physical-core order (0-5 Cortex-A55,
    // 6-7 Cortex-A76), one homogeneous cluster. QEMU keeps a single CPU: its
    // MPIDRs differ from sweet.
    let core_count = if opts.qemu { 1 } else { 8 };
    let cpu_nodes = (0..core_count)
        .map(|core| {
            node(
                &format!("cpu{core}"),
                vec![
                    ph!(),
                    ("device_type", adt::cstr("cpu")),
                    ("compatible", [adt::cstr("qcom,kryo470"), adt::cstr("ARM,v8")].concat()),
                    ("reg", u32v(&[core << 8])), // actual MPIDR Aff1:Aff0, NOT logical ID
                    ("cpu-id", u32v(&[core as u32])),
                    ("cluster-id", u32v(&[0])),
                    ("cluster-type", u32v(&[0])), // homogeneous CLUSTER_TYPE_SMP
                    ("die-cluster-id", u32v(&[0])),
                    ("cluster-core-id", u32v(&[core as u32])),
                    ("l2-cache-id", u32v(&[core as u32])),
                    ("l3-cache-id", u32v(&[0])),
                    ("state", adt::cstr(if core == 0 { "running" } else { "stopped" })),
                    ("timebase-frequency", u32v(&[timebase])),
                    (
                        "clock-frequency",
                        u32v(&[if core >= 6 { 2_304_000_000 } else { 1_804_800_000 }]),
                    ),
                    ("bus-frequency", u32v(&[100_000_000])),
                    ("memory-frequency", u32v(&[1_866_000_000])),
                    ("peripheral-frequency", u32v(&[timebase])),
                    ("fixed-frequency", u32v(&[timebase])),
                ],
                vec![],
            )
        })
        .collect();
    let cpus = node(
        "cpus",
        vec![ph!(), ("#address-cells", u32v(&[1])), ("#size-cells", u32v(&[0]))],
        cpu_nodes,
    );

    let gic = node(
        "gic",
        vec![
            ph!(),
            ("compatible", adt::cstr("arm,gic-v3")),
            ("interrupt-controller", vec![]),
            (
                "virtual-timer-intid",
                u32v(&[if opts.qemu { QEMU_VTIMER_INTID } else { VTIMER_INTID }]),
            ),
            ("reg", u64v(&[gicd - SOC_BASE, GICD_SIZE, gicr - SOC_BASE, GICR_SIZE])),
        ],
        vec![],
    );
    // Main display node. Two personas share the node:
    //   * default: the SWEET console persona (`sweet,display` + canvas props,
    //     matched by the SWEET-patched AppleCLCD, patch 0046) — this is what
    //     boots on every proven config;
    //   * --display: the REAL D22AP persona adds `disp0,t8015` + the 4-range
    //     register map + the D22AP display properties, so the real IOMFB
    //     driver stack (IOMobileFramebuffer / AppleMobileDispH10P) matches.
    let mut disp0 = node(
        "disp0",
        vec![
            ph!(),
            ("device_type", adt::cstr("dart")),
            ("compatible", adt::cstr("sweet,display")),
            ("canvas-width", u32v(&[1125])),
            ("canvas-height", u32v(&[2436])),
        ],
        vec![],
    );

    let mut dart_disp0 = None;
    let mut mipi_dsim = None;

    if opts.display {
        // --- mapper phandle first: disp0.iommu-parent references it. ------
        let mapper_phandle = phandle.next().unwrap();
        let dart_phandle = phandle.next().unwrap();
        let disp_phandle = phandle.next().unwrap();
        // disp0's own phandle was allocated by ph!() above; overwrite it with
        // our deterministic one so iommu-parent wiring is verifiable.
        disp0
            .props
            .retain(|(k, _, _)| k != "AAPL,phandle");
        disp0.props.push(("AAPL,phandle".to_string(), 0, u32v(&[disp_phandle])));

        // Real D22AP disp0 properties (devicetree-d22ap.md lines 1880-1908).
        // OMITTED on purpose:
        //   * interrupt-parent — the real node points at the AIC (phandle
        //     0x1c), which does not exist on this synthetic tree. MEASURED
        //     (this session): an `interrupts` array WITHOUT interrupt-parent
        //     is NOT inert — IOKit resolves it against the ROOT interrupt
        //     controller (our arm,gic-v3 node), and nub creation wedges the
        //     boot before ANY IOKit output (console freezes after
        //     "configuring debugging over serial", kernel parked in
        //     _ml_get_timebase, zero device register accesses). With no
        //     interrupts property the boot reaches the M2 mount wall.
        //     Stage 2 reintroduces interrupts together with an AIC model
        //     (or GIC-mapped IRQ numbers) as a deliberate step.
        for (k, v) in [
            ("compatible", [adt::cstr("disp0,t8015"), adt::cstr("sweet,display")].concat()),
            (
                "reg",
                u32v(&[
                    0x1800_0000, 0x2a_6000, // main display engine
                    0x1870_8000, 0x1000,   // disp0 aux
                    0x320b_8000, 0x1000,   // pmgr-adjacent
                    0x3230_c000, 0x1000,   // pmgr-adjacent
                ]),
            ),
            // (interrupts/interrupt-parent deliberately omitted — see the
            // block comment above; IOKit would resolve them against the GIC
            // and wedge nub creation)
            ("iommu-parent", u32v(&[mapper_phandle])),
            ("clock-gates", u64v(&[0x7600_0000_fe, 0x7300_0000_74])),
            ("clock-ids", u64v(&[0x13b0_0000_013d])),
            ("power-gates", u32v(&[0xfe])),
            ("max-avg-bpp", u32v(&[0xd_999a])),
            ("max-peak-bpp", u32v(&[0xd_999a])),
            ("max-scaling-ratio", u32v(&[0x2_0000])),
            ("subframe-interrupt-time-lines", u32v(&[0x9bb])),
            ("subframe-duration-nclks", u32v(&[0x61b4d])),
            ("display-lead-time-nclks", u32v(&[0x54b0])),
            (
                "first-frame-response-threshold",
                u32v(&[0xccccd, 0xe147b, 0xf5c29, 0x10a3d7, 0x170a3d]),
            ),
            ("display-carveout-inhibitcache", u32v(&[1])),
        ] {
            disp0.set_prop(k, v);
        }

        // Real D22AP dart-disp0 (devicetree-d22ap.md lines 1910-1938).
        let mut mapper = node(
            "mapper-disp0",
            vec![
                ("device_type", adt::cstr("dart-mapper")),
                ("compatible", adt::cstr("iommu-mapper")),
                ("AAPL,phandle", u32v(&[mapper_phandle])),
                ("iomd-cache-size", u32v(&[0x40])),
                ("iomd-cache-ttl", u32v(&[0x7d0])),
                ("tail-padding", u32v(&[1])),
            ],
            vec![],
        );
        // reg is a run-time placeholder in the real tree (4 zero bytes).
        mapper.set_prop("reg", vec![0u8; 4]);

        dart_disp0 = Some(node(
            "dart-disp0",
            vec![
                ("device_type", adt::cstr("dart")),
                ("compatible", adt::cstr("dart,t8015")),
                ("AAPL,phandle", u32v(&[dart_phandle])),
                ("reg", u32v(&[0x1870_4000, 0x4000, 0x1870_0000, 0x4000])),
                ("sids", u32v(&[3])),
                ("page-size", u32v(&[0x1000])),
                ("stt-idx-width", u32v(&[0xa])),
                ("dart-options", u32v(&[5])),
                ("smmu-options", u32v(&[2])),
                ("fetch-config", u32v(&[0xe0303])),
                ("diag-config", u32v(&[0x8000_0100])),
                // instance = "TRADDART\0\0\0\0" "UMMSSMMU\0\0\0\0" (24 bytes)
                (
                    "instance",
                    [b"TRADDART".as_slice(), &[0u8; 4], b"UMMSSMMU".as_slice(), &[0u8; 4]].concat(),
                ),
                ("clock-gates", [b"%".as_slice(), &[0u8; 3]].concat()),
                ("power-gates", [b"%".as_slice(), &[0u8; 3]].concat()),
                ("smmu-clock-gating", vec![]),
                ("bypass-address", u32v(&[0x800, 0, 0, 0, 0x800])),
                ("error-reflector", u64v(&[0x2_33ff_c000, 0x2_33ff_c000])),
                // interrupts (0xdf, 0xde) omitted — see disp0 block comment
            ],
            vec![mapper],
        ));

        // Real D22AP mipi-dsim (devicetree-d22ap.md lines 1839-1878).
        // OMITTED on purpose: sac-frequency-list (240 bytes, truncated in the
        // real dump), phy-test (138 zero bytes), function-* phandle refs,
        // interrupt-parent (same AIC reason as disp0).
        let lcd = node(
            "lcd",
            vec![
                ("device_type", adt::cstr("lcd")),
                ("compatible", adt::cstr("lcd,summit")),
                ("video-mode", u32v(&[1])),
                ("backlight-level-encoding", u32v(&[0xb])),
                ("set-temp-require-mcs-unlock", u32v(&[1])),
                ("power-to-reset-delay", u32v(&[0xa])),
            ],
            vec![],
        );
        mipi_dsim = Some(node(
            "mipi-dsim",
            vec![
                ("device_type", adt::cstr("mipi-dsim")),
                ("compatible", adt::cstr("mipi-dsim-1,synopsys")),
                ("reg", u32v(&[0x1840_0000, 0x10_0000])),
                ("mipi-version", u32v(&[5])),
                ("#lanes", u32v(&[2])),
                ("clock-ids", u32v(&[0x111])),
                // interrupts (0xe1) omitted — see disp0 block comment
                ("clock-gates", u32v(&[0x71])),
                ("power-gates", u32v(&[0x71])),
                ("invert-interrupt-mask", vec![]),
                ("sac-enable", u32v(&[1])),
                ("sac-version", u32v(&[3])),
                ("sac-divisor", u32v(&[8])),
                ("sac-noise-width", u32v(&[0x131_2d00])),
                ("sac-frequency-list-count", u32v(&[0xa])),
                ("agile-line-count", u32v(&[9])),
                ("apply-testcode", u32v(&[1])),
                ("mipi-top-size", [b"\\".as_slice(), &[0u8; 3]].concat()),
            ],
            vec![lcd],
        ));
    }

    let mut arm_io_children = vec![
        gic,
        // watchdogd checks IODeviceTree:/arm-io/wdt for no-pmu and
        // otherwise crashes trying to set the PMU boot stage.
        node("wdt", vec![ph!(), ("no-pmu", u32v(&[1]))], vec![]),
        disp0,
    ];
    if let Some(dart) = dart_disp0 {
        arm_io_children.push(dart);
    }
    if let Some(mipi) = mipi_dsim {
        arm_io_children.push(mipi);
    }
    let arm_io = node(
        "arm-io",
        vec![
            ph!(),
            ("device_type", adt::cstr("sweet-io")),
            ("compatible", [adt::cstr("sweet-io"), adt::cstr("arm-io")].concat()),
            ("#address-cells", u32v(&[2])),
            ("#size-cells", u32v(&[2])),
            ("ranges", u64v(&[0, SOC_BASE, 0x1_0000_0000 - SOC_BASE])),
        ],
        arm_io_children,
    );

    let fbcon = node(
        "sweet-fbcon",
        vec![
            ph!(),
            ("reg", u64v(&[FB, FB_SIZE])),
            ("width", u32v(&[FB_W])),
            ("height", u32v(&[FB_H])),
            ("stride", u32v(&[FB_STRIDE])),
        ],
        vec![],
    );

    // bsd/dev/sweet_ufs.c: the UFS host ABL leaves running (QEMU has none).
    let ufs = node(
        "sweet-ufs",
        vec![ph!(), ("compatible", adt::cstr("qcom,ufshc")), ("reg", u64v(&[UFS, UFS_SIZE]))],
        vec![],
    );

    // pmap_compute_max_asids requires the node even when no overrides are set.
    // keybagd reads /private/var/keybags/systembag.kb as a plain plist.
    let defaults = node("defaults", vec![ph!(), ("no-effaceable-storage", u32v(&[1]))], vec![]);

    // MobileGestalt HomeButtonType (D22AP: 2, no Home button).
    let buttons = node(
        "buttons",
        vec![
            ph!(),
            (
                "button-names",
                [adt::cstr("volup"), adt::cstr("voldown"), adt::cstr("hold")].concat(),
            ),
            ("home-button-type", u32v(&[2])),
            ("opposed-power-vol-buttons", u32v(&[1])),
        ],
        vec![],
    );

    // Panic log region. /pram reg = <phys size>, just above the exposed range.
    let pram = node(
        "pram",
        vec![ph!(), ("reg", u64v(&[panic_base, PANIC_LOG_SIZE]))],
        vec![],
    );

    let mut children = vec![chosen, defaults, cpus, arm_io, fbcon, buttons, pram];
    if let Some(product) = opts.product {
        let mut n = Node::new("product");
        n.props.push(("AAPL,phandle".to_string(), 0, u32v(&[phandle.next().unwrap()])));
        for (k, v) in product {
            n.props.push((k.clone(), 0, v.clone()));
        }
        children.push(n);
    }
    if !opts.qemu {
        children.push(ufs);
    }

    // Identity of the iPhone X whose userland runs here: compatible[0] is
    // hw.model/hw.target (D22AP), model is hw.machine/hw.product (iPhone10,3).
    Ok(node(
        "device-tree",
        vec![
            ph!(),
            (
                "compatible",
                [
                    adt::cstr("D22AP"),
                    adt::cstr("iPhone10,3"),
                    adt::cstr("xiaomi,sweet"),
                    adt::cstr("qcom,sm7150"),
                    adt::cstr("AppleARM"),
                ]
                .concat(),
            ),
            ("model", adt::cstr("iPhone10,3")),
            ("target-type", adt::cstr("D22")),
            ("target-sub-type", adt::cstr("D22AP")),
            ("manufacturer", adt::cstr("Apple Inc.")),
            ("#address-cells", u32v(&[2])),
            ("#size-cells", u32v(&[2])),
        ],
        children,
    ))
}

/// Parse ADT, find /chosen/dram-base + dram-size. Port of `mkpayload.adt_dram`.
pub fn adt_dram(adt_blob: &[u8]) -> Result<(u64, u64)> {
    let (root, _) = adt::parse(adt_blob)?;
    let chosen = root
        .child("chosen")
        .ok_or_else(|| crate::Error::Msg("ADT has no /chosen".into()))?;
    let base = chosen
        .prop("dram-base")
        .and_then(|v| <[u8; 8]>::try_from(v).ok())
        .map(u64::from_le_bytes);
    let size = chosen
        .prop("dram-size")
        .and_then(|v| <[u8; 8]>::try_from(v).ok())
        .map(u64::from_le_bytes);
    match (base, size) {
        (Some(b), Some(s)) => Ok((b, s)),
        _ => bail!("ADT /chosen lacks dram-base/dram-size"),
    }
}

/// Add /chosen/memory-map "RAMDisk" = <phys, size> (two uintptr_t). Port of
/// `mkpayload.add_ramdisk_to_adt` — IOFindBSDRoot turns it into md0.
pub fn add_ramdisk_to_adt(adt_blob: &mut Vec<u8>, phys: u64, size: u64) -> Result<()> {
    let (mut root, _) = adt::parse(adt_blob)?;
    let chosen = root
        .children
        .iter_mut()
        .find(|c| c.name() == "chosen")
        .ok_or_else(|| crate::Error::Msg("ADT has no /chosen".into()))?;
    let mmap = chosen
        .children
        .iter_mut()
        .find(|c| c.name() == "memory-map")
        .ok_or_else(|| crate::Error::Msg("ADT /chosen has no /memory-map".into()))?;
    if mmap.prop("RAMDisk").is_some() {
        bail!("ADT already has a RAMDisk");
    }
    let mut val = Vec::with_capacity(16);
    val.extend_from_slice(&phys.to_le_bytes());
    val.extend_from_slice(&size.to_le_bytes());
    mmap.set_prop("RAMDisk", val);
    *adt_blob = adt::serialize(&root);
    Ok(())
}

// ---------------------------------------------------------------------------
// SHA-384 (only for the fixed boot-manifest stand-in; no deps).
// ---------------------------------------------------------------------------

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512_block(h: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u64; 80];
    for i in 0..16 {
        let mut v = [0u8; 8];
        v.copy_from_slice(&block[i * 8..i * 8 + 8]);
        w[i] = u64::from_be_bytes(v);
    }
    for i in 16..80 {
        let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
        let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..80 {
        let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K512[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    let add = [a, b, c, d, e, f, g, hh];
    for (i, v) in add.iter().enumerate() {
        h[i] = h[i].wrapping_add(*v);
    }
}

/// SHA-384 of `data` (used for /chosen/boot-manifest-hash).
pub fn sha384(data: &[u8]) -> [u8; 48] {
    let mut h: [u64; 8] = [
        0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939,
        0x67332667ffc00b31, 0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
    ];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u128) * 8;
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for block in msg.chunks(128) {
        sha512_block(&mut h, block);
    }
    let mut out = [0u8; 48];
    for (i, v) in h.iter().take(6).enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&v.to_be_bytes());
    }
    out
}
