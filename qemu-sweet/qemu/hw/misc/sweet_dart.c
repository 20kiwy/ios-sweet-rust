/*
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Apple T8015 DART (Device Address Resolution Table) -- stage-1 "sweet"
 * model for the ios-sweet-rust project.
 *
 * This is a deliberately honest stage-1 model:
 *
 *   * The exact t8015 DART bit layout is reverse-engineering grade; the
 *     register model below is store-and-echo with a handful of curated
 *     semantics so that the real iOS kernel driver (AppleT8015DART,
 *     compatible "dart,t8015") can probe the hardware and leave traces
 *     in the QEMU log.  Those traces are what stage 2 uses to implement
 *     the bit-accurate TCR/TTBR/TLB PTE walker.
 *
 *   * Stage 1 implements: per-SID TCR / TTBR0 registers with the
 *     TRANSLATE_ENABLE / BYPASS semantics documented below, an IOMMU
 *     MemoryRegion that currently translates identity (pass-through),
 *     a remap register window, and a reserved fault IRQ line that is
 *     never asserted.
 *
 *   * Everything else is a silent-read / logged-write store+echo
 *     register file so read-back checks by the kernel driver pass.
 *
 * Observability contract: the Rust runner boots with
 * `-d int,guest_errors,unimp -D qemu.log`, so ALL logging goes through
 * qemu_log_mask(LOG_UNIMP|LOG_GUEST_ERROR) with the "sweet-dart:" prefix.
 * No trace-events, no printf.
 */

#include "qemu/osdep.h"
#include "exec/address-spaces.h"
#include "hw/arm/sweet_devices.h"
#include "hw/irq.h"
#include "hw/qdev-properties.h"
#include "hw/sysbus.h"
#include "qapi/error.h"
#include "qemu/log.h"
#include "qemu/module.h"

/* ------------------------------------------------------------------ */
/* Register map (stage-1 understanding, subject to kernel traces)      */
/* ------------------------------------------------------------------ */

/* Per-SID register block stride. */
#define SWEET_DART_SID_STRIDE   0x100ull
#define SWEET_DART_TCR_OFF(sid)   (0x000ull + SWEET_DART_SID_STRIDE * (sid))
#define SWEET_DART_TTBR0_OFF(sid, n) \
    (0x200ull + SWEET_DART_SID_STRIDE * (sid) + 8ull * (n))

/*
 * TCR bits (stage-1 semantics, documented for the stage-2 PTE walker):
 *   bit 0: TRANSLATE_ENABLE -- when clear, DMA passes through untouched.
 *   bit 1: BYPASS          -- when set, DMA passes through even if
 *                             translation is enabled.
 * Every other bit is stored and echoed.
 */
#define SWEET_DART_TCR_TRANSLATE_ENABLE (1ull << 0)
#define SWEET_DART_TCR_BYPASS           (1ull << 1)
/*
 * Reset value: bypass on, translate off.  Mirrors a DART left in bypass
 * by iBoot, so DMA works out of the box on stage 1.
 */
#define SWEET_DART_TCR_RESET            SWEET_DART_TCR_BYPASS

#define SWEET_DART_MAX_SIDS    16
#define SWEET_DART_TTBR_SLOTS  4

/* ------------------------------------------------------------------ */
/* State                                                               */
/* ------------------------------------------------------------------ */

struct SweetDartState {
    SysBusDevice parent_obj;

    /* Main register window @ base and remap window @ remap-base.
     * Both self-mapped into the system address space (priority 1). */
    MemoryRegion mmio;
    MemoryRegion mmio_remap;

    /*
     * IOMMU MemoryRegion (TYPE_SWEET_DART_IOMMU_MEMORY_REGION).  NOT
     * mapped into the system address space -- it is only referenced as
     * the root of DMA AddressSpaces (see sweet_disp0.c).
     */
    IOMMUMemoryRegion iommu;

    /* Fault IRQ line: wired via sysbus_init_irq but never asserted in
     * stage 1 (we only log DMA faults).  Stage 2 will drive it from the
     * real fault registers. */
    qemu_irq irq;

    /* Properties. */
    uint64_t base;        /* main window base  (default 0x18704000) */
    uint64_t remap_base;  /* remap window base (default 0x18700000) */
    uint32_t size;        /* window size        (default 0x4000)    */
    uint32_t sids;        /* stream IDs         (default 3, max 16) */

    /* Stage-1 translation state.  The DMA requester SID is not modelled
     * yet; translation uses the global / SID 0 view. */
    uint64_t *tcr;        /* sids entries */
    uint64_t *ttbr0;      /* sids * SWEET_DART_TTBR_SLOTS entries */
    bool pass_logged;     /* "stage-1 pass-through" logged once per enable */

    /* Byte-granular store+echo backing arrays (LE layout). */
    uint8_t *store;
    uint8_t *store_remap;

    /* Latched fault info (exposed to stage 2; not readable yet). */
    hwaddr fault_iova;
};

DECLARE_INSTANCE_CHECKER(SweetDartState, SWEET_DART, TYPE_SWEET_DART)

/* ------------------------------------------------------------------ */
/* Store+echo helpers (little-endian byte order, portable)             */
/* ------------------------------------------------------------------ */

static uint64_t sweet_store_read_le(const uint8_t *store, hwaddr addr,
                                    unsigned size)
{
    uint64_t val = 0;
    unsigned i;

    for (i = 0; i < size; i++) {
        val |= (uint64_t)store[addr + i] << (8 * i);
    }
    return val;
}

static void sweet_store_write_le(uint8_t *store, hwaddr addr, uint64_t val,
                                 unsigned size)
{
    unsigned i;

    for (i = 0; i < size; i++) {
        store[addr + i] = (uint8_t)(val >> (8 * i));
    }
}

/* ------------------------------------------------------------------ */
/* Register matching                                                   */
/* ------------------------------------------------------------------ */

/* TCR(sid): 4-byte access at the TCR offset (an 8-byte access there also
 * hits TCR in its low half; we accept both so the shadow can never go
 * stale). */
static bool dart_tcr_hit(SweetDartState *s, hwaddr addr, unsigned size,
                         uint32_t *sid)
{
    uint32_t i;

    if (size != 4 && size != 8) {
        return false;
    }
    for (i = 0; i < s->sids; i++) {
        if (addr == SWEET_DART_TCR_OFF(i)) {
            *sid = i;
            return true;
        }
    }
    return false;
}

/* TTBR0(sid)[slot]: access starting exactly at the slot offset,
 * 4 or 8 bytes wide. */
static bool dart_ttbr_hit(SweetDartState *s, hwaddr addr, unsigned size,
                          uint32_t *sid, uint32_t *slot)
{
    uint32_t i, n;

    if (size != 4 && size != 8) {
        return false;
    }
    for (i = 0; i < s->sids; i++) {
        for (n = 0; n < SWEET_DART_TTBR_SLOTS; n++) {
            if (addr == SWEET_DART_TTBR0_OFF(i, n)) {
                *sid = i;
                *slot = n;
                return true;
            }
        }
    }
    return false;
}

/* ------------------------------------------------------------------ */
/* Main window                                                         */
/* ------------------------------------------------------------------ */

/* Reads are intentionally silent: reads flood logs.  TCR/TTBR values live
 * in the same backing store, so kernel read-back checks pass. */
static uint64_t sweet_dart_mmio_read(void *opaque, hwaddr addr, unsigned size)
{
    SweetDartState *s = SWEET_DART(opaque);

    return sweet_store_read_le(s->store, addr, size);
}

static void sweet_dart_mmio_write(void *opaque, hwaddr addr, uint64_t val,
                                  unsigned size)
{
    SweetDartState *s = SWEET_DART(opaque);
    uint32_t sid, slot;

    if (dart_tcr_hit(s, addr, size, &sid)) {
        uint64_t old = s->tcr[sid];

        s->tcr[sid] = val & 0xffffffffull;
        /* Keep the backing store in sync so read-back matches. */
        sweet_store_write_le(s->store, addr, s->tcr[sid], 4);
        if (old != s->tcr[sid]) {
            qemu_log_mask(LOG_UNIMP,
                          "sweet-dart: W TCR[%u] = 0x%04" PRIx64 "\n",
                          sid, s->tcr[sid]);
        }
        if (!(old & SWEET_DART_TCR_TRANSLATE_ENABLE) &&
            (s->tcr[sid] & SWEET_DART_TCR_TRANSLATE_ENABLE)) {
            /* Loud, once per enable. */
            qemu_log_mask(LOG_UNIMP,
                          "sweet-dart: TCR[%u] TRANSLATE_ENABLE set: "
                          "stage-1 pass-through (PTE walker TODO)\n", sid);
            s->pass_logged = false;
        }
        return;
    }

    if (dart_ttbr_hit(s, addr, size, &sid, &slot)) {
        s->ttbr0[sid * SWEET_DART_TTBR_SLOTS + slot] = val;
        sweet_store_write_le(s->store, addr, val, size);
        qemu_log_mask(LOG_UNIMP,
                      "sweet-dart: W TTBR0[%u][%u] = 0x%016" PRIx64 "\n",
                      sid, slot, val);
        return;
    }

    /* Unknown offset: store+echo, one compact log line. */
    sweet_store_write_le(s->store, addr, val, size);
    qemu_log_mask(LOG_UNIMP,
                  "sweet-dart: W unk[0x%04" HWADDR_PRIx "] = 0x%08" PRIx64
                  " (store+echo)\n", addr, val);
}

/* ------------------------------------------------------------------ */
/* Remap window (store+echo only)                                      */
/* ------------------------------------------------------------------ */

static uint64_t sweet_dart_remap_read(void *opaque, hwaddr addr,
                                      unsigned size)
{
    SweetDartState *s = SWEET_DART(opaque);

    return sweet_store_read_le(s->store_remap, addr, size);
}

static void sweet_dart_remap_write(void *opaque, hwaddr addr, uint64_t val,
                                   unsigned size)
{
    SweetDartState *s = SWEET_DART(opaque);

    sweet_store_write_le(s->store_remap, addr, val, size);
    qemu_log_mask(LOG_UNIMP,
                  "sweet-dart: remap W unk[0x%04" HWADDR_PRIx "] = 0x%08"
                  PRIx64 " (store+echo)\n", addr, val);
}

/* ------------------------------------------------------------------ */
/* MemoryRegionOps                                                     */
/* ------------------------------------------------------------------ */

static const MemoryRegionOps sweet_dart_mmio_ops = {
    .read = sweet_dart_mmio_read,
    .write = sweet_dart_mmio_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .valid.min_access_size = 4,
    .valid.max_access_size = 8,
    .impl.min_access_size = 4,
    .impl.max_access_size = 8,
};

static const MemoryRegionOps sweet_dart_remap_ops = {
    .read = sweet_dart_remap_read,
    .write = sweet_dart_remap_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .valid.min_access_size = 4,
    .valid.max_access_size = 8,
    .impl.min_access_size = 4,
    .impl.max_access_size = 8,
};

/* ------------------------------------------------------------------ */
/* IOMMU (stage 1: identity / pass-through, deny on unconfigured TTBR) */
/* ------------------------------------------------------------------ */

static IOMMUTLBEntry sweet_dart_iommu_translate(IOMMUMemoryRegion *iommu,
                                                hwaddr addr,
                                                IOMMUAccessFlags flag,
                                                int iommu_idx)
{
    SweetDartState *s = container_of(iommu, SweetDartState, iommu);
    /* Stage 1: the DMA requester SID is not carried yet; use the global
     * (SID 0) view. */
    const uint32_t sid = 0;
    uint64_t tcr = s->tcr[sid];
    IOMMUTLBEntry ret = {
        .target_as = &address_space_memory,
        .iova = addr,
        .translated_addr = addr & ~0xfffull,
        .addr_mask = 0xfff,
        .perm = IOMMU_RW,
    };
    bool allow;

    if (!(tcr & SWEET_DART_TCR_TRANSLATE_ENABLE) ||
        (tcr & SWEET_DART_TCR_BYPASS)) {
        /* Bypass enabled or translation disabled: identity. */
        allow = true;
    } else if (s->ttbr0[sid * SWEET_DART_TTBR_SLOTS + 0] != 0) {
        /* Translate enabled with a non-zero TTBR: stage-1 pass-through.
         * The real PTE walk is the stage-2 deliverable; log once per
         * enable so the traces tell us when the kernel turned the DART on. */
        if (!s->pass_logged) {
            qemu_log_mask(LOG_UNIMP,
                          "sweet-dart: translate enabled, stage-1 "
                          "pass-through (PTE walker TODO)\n");
            s->pass_logged = true;
        }
        allow = true;
    } else {
        allow = false;
    }

    if (!allow) {
        s->fault_iova = addr;
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-dart: DMA fault iova=0x%" HWADDR_PRIx
                      " (TTBR0[0]=0, translation on)\n", addr);
        ret.perm = IOMMU_NONE;
        ret.translated_addr = 0;
    }
    return ret;
}

static uint64_t sweet_dart_iommu_get_min_page_size(IOMMUMemoryRegion *iommu)
{
    return 4096;
}

static void sweet_dart_iommu_class_init(ObjectClass *klass, void *data)
{
    IOMMUMemoryRegionClass *imrc = IOMMU_MEMORY_REGION_CLASS(klass);

    imrc->translate = sweet_dart_iommu_translate;
    imrc->get_min_page_size = sweet_dart_iommu_get_min_page_size;
}

static const TypeInfo sweet_dart_iommu_info = {
    .name = TYPE_SWEET_DART_IOMMU_MEMORY_REGION,
    .parent = TYPE_IOMMU_MEMORY_REGION,
    .class_init = sweet_dart_iommu_class_init,
};

/* ------------------------------------------------------------------ */
/* Device plumbing                                                     */
/* ------------------------------------------------------------------ */

MemoryRegion *sweet_dart_iommu_mr(Object *dart_obj)
{
    SweetDartState *s = SWEET_DART(dart_obj);

    return MEMORY_REGION(&s->iommu);
}

static void sweet_dart_realize(DeviceState *dev, Error **errp)
{
    SweetDartState *s = SWEET_DART(dev);

    if (s->sids == 0 || s->sids > SWEET_DART_MAX_SIDS) {
        error_setg(errp, "sweet-dart: property sids must be 1..%u",
                   SWEET_DART_MAX_SIDS);
        return;
    }
    if (s->size < 0x400 || (s->size & (s->size - 1)) != 0) {
        error_setg(errp, "sweet-dart: property size must be a power of two "
                   ">= 0x400");
        return;
    }

    s->tcr = g_new0(uint64_t, s->sids);
    s->ttbr0 = g_new0(uint64_t, s->sids * SWEET_DART_TTBR_SLOTS);
    s->store = g_malloc0(s->size);
    s->store_remap = g_malloc0(s->size);

    memory_region_init_io(&s->mmio, OBJECT(s), &sweet_dart_mmio_ops, s,
                          "sweet-dart", s->size);
    memory_region_init_io(&s->mmio_remap, OBJECT(s), &sweet_dart_remap_ops, s,
                          "sweet-dart-remap", s->size);
    memory_region_init_iommu(&s->iommu, sizeof(s->iommu),
                             TYPE_SWEET_DART_IOMMU_MEMORY_REGION, OBJECT(s),
                             "dart-iommu", 0x100000000ULL);

    /*
     * Self-map both register windows into the system address space.
     * Priority 1 beats the (empty) pcie-mmio alias container that 'virt'
     * puts at 0x10000000..0x3effffff, so no hw/arm/virt.c change is
     * needed.  All D22AP display bases are below RAM (0x40000000) or in
     * that window.
     */
    memory_region_add_subregion_overlap(get_system_memory(), s->base,
                                        &s->mmio, 1);
    memory_region_add_subregion_overlap(get_system_memory(), s->remap_base,
                                        &s->mmio_remap, 1);

    qemu_log_mask(LOG_UNIMP,
                  "sweet-dart: realized base=0x%" PRIx64 " remap=0x%" PRIx64
                  " size=0x%x sids=%u (stage-1 bypass model)\n",
                  s->base, s->remap_base, s->size, s->sids);
}

static void sweet_dart_reset(DeviceState *dev)
{
    SweetDartState *s = SWEET_DART(dev);
    uint32_t i;

    if (!s->store) {
        return; /* not realized yet */
    }

    memset(s->ttbr0, 0, s->sids * SWEET_DART_TTBR_SLOTS * sizeof(uint64_t));
    memset(s->store, 0, s->size);
    memset(s->store_remap, 0, s->size);
    for (i = 0; i < s->sids; i++) {
        s->tcr[i] = SWEET_DART_TCR_RESET;
        /* Mirror the reset value into the backing store so read-back of
         * TCR matches before the kernel writes anything. */
        sweet_store_write_le(s->store, SWEET_DART_TCR_OFF(i),
                             SWEET_DART_TCR_RESET, 4);
    }
    s->pass_logged = false;
}

static void sweet_dart_instance_init(Object *obj)
{
    SweetDartState *s = SWEET_DART(obj);

    /* Fault IRQ: reserved for stage 2, never asserted here. */
    sysbus_init_irq(SYS_BUS_DEVICE(obj), &s->irq);
}

static const Property sweet_dart_properties[] = {
    DEFINE_PROP_UINT64("base", SweetDartState, base, 0x18704000),
    DEFINE_PROP_UINT64("remap-base", SweetDartState, remap_base, 0x18700000),
    DEFINE_PROP_UINT32("size", SweetDartState, size, 0x4000),
    DEFINE_PROP_UINT32("sids", SweetDartState, sids, 3),
};

static void sweet_dart_class_init(ObjectClass *oc, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(oc);

    dc->realize = sweet_dart_realize;
    dc->user_creatable = true;
    device_class_set_legacy_reset(dc, sweet_dart_reset);
    device_class_set_props(dc, sweet_dart_properties);
    dc->desc = "Apple T8015 DART (dart,t8015) stage-1 model";
    set_bit(DEVICE_CATEGORY_MISC, dc->categories);
}

static const TypeInfo sweet_dart_info = {
    .name = TYPE_SWEET_DART,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(SweetDartState),
    .instance_init = sweet_dart_instance_init,
    .class_init = sweet_dart_class_init,
};

static void sweet_dart_register_types(void)
{
    type_register_static(&sweet_dart_info);
    type_register_static(&sweet_dart_iommu_info);
}

type_init(sweet_dart_register_types)
