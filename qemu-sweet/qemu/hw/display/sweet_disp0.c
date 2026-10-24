/*
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Apple T8015 display controller (disp0,t8015) -- stage-1 "sweet" model
 * for the ios-sweet-rust project.
 *
 * Stage-1 scope (honest):
 *   * Four MMIO windows self-mapped at the D22AP (iPhone X) addresses:
 *       main  0x18000000  size 0x2a6000   (display controller core)
 *       aux   0x18708000  size 0x1000     (disp0-aux)
 *       pmgrA 0x320b8000  size 0x1000     (disp0 pmgr gate block A)
 *       pmgrB 0x3230c000  size 0x1000     (disp0 pmgr gate block B)
 *   * Register semantics are store+echo with a few curated constants so
 *     AppleSynopsysMIPIDSIController / disp0 kernel probing finds sane
 *     values.  The bit-accurate register model is stage 2 (driven by the
 *     kernel traces this model produces).
 *   * DMA goes through the linked DART's IOMMU address space (stage 1:
 *     identity translation), proving the wiring end to end.
 *   * No IRQ is ever asserted.
 *   * A host-driven swap hook (HMP `sweet-disp0-swap`) blits a BGRA
 *     surface into the framebuffer through the DART address space.
 *
 * Observability contract: `-d int,guest_errors,unimp -D qemu.log`, all
 * traces via qemu_log_mask(LOG_UNIMP|LOG_GUEST_ERROR) with the
 * "sweet-disp0:" prefix.  Reads are silent (they flood).
 */

#include "qemu/osdep.h"
#include "exec/address-spaces.h"
#include "exec/cpu-common.h"
#include "hw/arm/sweet_devices.h"
#include "hw/irq.h"
#include "hw/qdev-properties.h"
#include "hw/sysbus.h"
#include "monitor/monitor.h"
#include "qapi/error.h"
#include "qobject/qdict.h"
#include "qemu/log.h"
#include "qemu/module.h"
#include "system/dma.h"

/* Window sizes. */
#define SWEET_DISP0_MAIN_SIZE   0x2a6000
#define SWEET_DISP0_SMALL_SIZE  0x1000

/* Curated stage-1 constants (documented in the README). */
#define SWEET_DISP0_ID_VERSION  0x1d020005ull  /* main+0x0 id/version */

/* Main-window offsets. */
#define SWEET_DISP0_REG_ID        0x00
#define SWEET_DISP0_REG_INTMASK   0x40
#define SWEET_DISP0_REG_INTSTATUS 0x48
#define SWEET_DISP0_REG_SURFACE   0x100  /* host-managed: last swap PA */

typedef struct SweetDisp0State SweetDisp0State;

/* One store+echo bank; carries its name for log lines. */
typedef struct SweetStoreBank {
    struct SweetDisp0State *dev;
    const char *name;
    uint8_t *data;
} SweetStoreBank;

struct SweetDisp0State {
    SysBusDevice parent_obj;

    /* Optional link to TYPE_SWEET_DART (-device sweet-disp0,dart=...). */
    Object *dart_obj;
    AddressSpace *dma_as;   /* DART IOMMU AS, or &address_space_memory */
    bool via_dart;

    /* Reserved: no IRQ lines are wired or asserted in stage 1. */
    qemu_irq irq;

    MemoryRegion mmio_main;
    MemoryRegion mmio_aux;
    MemoryRegion mmio_pmgr_a;
    MemoryRegion mmio_pmgr_b;

    SweetStoreBank bank_main;
    SweetStoreBank bank_aux;
    SweetStoreBank bank_pmgr_a;
    SweetStoreBank bank_pmgr_b;

    /* Properties. */
    uint64_t main_base;
    uint64_t aux_base;
    uint64_t pmgr_a_base;
    uint64_t pmgr_b_base;
    uint64_t fb_base;
    uint32_t fb_width;
    uint32_t fb_height;

    /* Curated stage-1 register state. */
    uint32_t int_mask;      /* 0x40 INTMASK (store+echo) */
    uint32_t int_status;    /* 0x48 INTSTATUS (W1C, never self-asserts) */
    uint64_t last_surface;  /* 0x100, set by hmp_sweet_disp0_swap */
};

DECLARE_INSTANCE_CHECKER(SweetDisp0State, SWEET_DISP0, TYPE_SWEET_DISP0)

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
/* Main window                                                         */
/* ------------------------------------------------------------------ */

/* Reads silent. */
static uint64_t sweet_disp0_main_read(void *opaque, hwaddr addr,
                                      unsigned size)
{
    SweetStoreBank *b = opaque;
    SweetDisp0State *s = b->dev;

    switch (addr) {
    case SWEET_DISP0_REG_ID:
        if (size == 4) {
            return SWEET_DISP0_ID_VERSION;
        }
        break;
    case SWEET_DISP0_REG_INTSTATUS:
        if (size == 4) {
            return s->int_status;
        }
        break;
    case SWEET_DISP0_REG_SURFACE:
        if (size == 8) {
            return s->last_surface;
        }
        if (size == 4) {
            /* 0x100: low half.  A 0x104 (high half) read falls through
             * to store+echo, which is fine for stage 1. */
            return (uint32_t)s->last_surface;
        }
        break;
    default:
        break;
    }

    return sweet_store_read_le(b->data, addr, size);
}

static void sweet_disp0_main_write(void *opaque, hwaddr addr, uint64_t val,
                                   unsigned size)
{
    SweetStoreBank *b = opaque;
    SweetDisp0State *s = b->dev;

    switch (addr) {
    case SWEET_DISP0_REG_ID:
        /* Read-only ID/version. */
        qemu_log_mask(LOG_UNIMP,
                      "sweet-disp0: W ID[0x0] (read-only, ignored) = 0x%08"
                      PRIx64 "\n", val);
        return;
    case SWEET_DISP0_REG_INTMASK:
        if (size == 4) {
            s->int_mask = (uint32_t)val;
            sweet_store_write_le(b->data, addr, val, size);
            qemu_log_mask(LOG_UNIMP,
                          "sweet-disp0: W INTMASK = 0x%08" PRIx64 "\n", val);
            return;
        }
        break;
    case SWEET_DISP0_REG_INTSTATUS:
        if (size == 4) {
            /* Write-1-to-clear; nothing ever self-asserts in stage 1. */
            s->int_status &= ~(uint32_t)val;
            if (val != 0) {
                qemu_log_mask(LOG_UNIMP,
                              "sweet-disp0: W1C INTSTATUS = 0x%08" PRIx64
                              "\n", val);
            }
            return;
        }
        break;
    case SWEET_DISP0_REG_SURFACE:
        /* Host-managed evidence register (sweet-disp0-swap). */
        qemu_log_mask(LOG_UNIMP,
                      "sweet-disp0: W SURFACE (host-managed, ignored) = "
                      "0x%016" PRIx64 "\n", val);
        return;
    default:
        break;
    }

    /* Unknown offset: store+echo, one compact log line. */
    sweet_store_write_le(b->data, addr, val, size);
    qemu_log_mask(LOG_UNIMP,
                  "sweet-disp0: main W unk[0x%04" HWADDR_PRIx "] = 0x%08"
                  PRIx64 " (store+echo)\n", addr, val);
}

static const MemoryRegionOps sweet_disp0_main_ops = {
    .read = sweet_disp0_main_read,
    .write = sweet_disp0_main_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .valid.min_access_size = 4,
    .valid.max_access_size = 8,
    .impl.min_access_size = 4,
    .impl.max_access_size = 8,
};

/* ------------------------------------------------------------------ */
/* Small windows (aux, pmgr-a, pmgr-b): pure store+echo                */
/* ------------------------------------------------------------------ */

/* Reads silent. */
static uint64_t sweet_disp0_small_read(void *opaque, hwaddr addr,
                                       unsigned size)
{
    SweetStoreBank *b = opaque;

    return sweet_store_read_le(b->data, addr, size);
}

static void sweet_disp0_small_write(void *opaque, hwaddr addr, uint64_t val,
                                    unsigned size)
{
    SweetStoreBank *b = opaque;

    sweet_store_write_le(b->data, addr, val, size);
    qemu_log_mask(LOG_UNIMP,
                  "sweet-disp0: %s W [0x%04" HWADDR_PRIx "] = 0x%08" PRIx64
                  " (store+echo)\n", b->name, addr, val);
}

static const MemoryRegionOps sweet_disp0_small_ops = {
    .read = sweet_disp0_small_read,
    .write = sweet_disp0_small_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .valid.min_access_size = 4,
    .valid.max_access_size = 8,
    .impl.min_access_size = 4,
    .impl.max_access_size = 8,
};

/* ------------------------------------------------------------------ */
/* Realize                                                             */
/* ------------------------------------------------------------------ */

static void sweet_disp0_realize(DeviceState *dev, Error **errp)
{
    SweetDisp0State *s = SWEET_DISP0(dev);

    if (s->fb_width == 0 || s->fb_height == 0) {
        error_setg(errp, "sweet-disp0: fb-width/fb-height must be nonzero");
        return;
    }

    s->bank_main.data = g_malloc0(SWEET_DISP0_MAIN_SIZE);
    s->bank_aux.data = g_malloc0(SWEET_DISP0_SMALL_SIZE);
    s->bank_pmgr_a.data = g_malloc0(SWEET_DISP0_SMALL_SIZE);
    s->bank_pmgr_b.data = g_malloc0(SWEET_DISP0_SMALL_SIZE);

    memory_region_init_io(&s->mmio_main, OBJECT(s), &sweet_disp0_main_ops,
                          &s->bank_main, "sweet-disp0-main",
                          SWEET_DISP0_MAIN_SIZE);
    memory_region_init_io(&s->mmio_aux, OBJECT(s), &sweet_disp0_small_ops,
                          &s->bank_aux, "sweet-disp0-aux",
                          SWEET_DISP0_SMALL_SIZE);
    memory_region_init_io(&s->mmio_pmgr_a, OBJECT(s), &sweet_disp0_small_ops,
                          &s->bank_pmgr_a, "sweet-disp0-pmgr-a",
                          SWEET_DISP0_SMALL_SIZE);
    memory_region_init_io(&s->mmio_pmgr_b, OBJECT(s), &sweet_disp0_small_ops,
                          &s->bank_pmgr_b, "sweet-disp0-pmgr-b",
                          SWEET_DISP0_SMALL_SIZE);

    /* Self-map all four windows (priority 1, see sweet_dart.c comment). */
    memory_region_add_subregion_overlap(get_system_memory(), s->main_base,
                                        &s->mmio_main, 1);
    memory_region_add_subregion_overlap(get_system_memory(), s->aux_base,
                                        &s->mmio_aux, 1);
    memory_region_add_subregion_overlap(get_system_memory(), s->pmgr_a_base,
                                        &s->mmio_pmgr_a, 1);
    memory_region_add_subregion_overlap(get_system_memory(), s->pmgr_b_base,
                                        &s->mmio_pmgr_b, 1);

    /* DMA address space: through the DART IOMMU when linked (stage 1:
     * DART translates identity), else straight system memory.  Log once
     * which path was taken. */
    if (s->dart_obj) {
        MemoryRegion *iommu = sweet_dart_iommu_mr(s->dart_obj);

        s->dma_as = g_new0(AddressSpace, 1);
        address_space_init(s->dma_as, iommu, "sweet-disp0-dma");
        s->via_dart = true;
        qemu_log_mask(LOG_UNIMP,
                      "sweet-disp0: DMA via DART iommu AS "
                      "\"sweet-disp0-dma\"\n");
    } else {
        s->dma_as = &address_space_memory;
        s->via_dart = false;
        qemu_log_mask(LOG_UNIMP,
                      "sweet-disp0: no DART linked, DMA via "
                      "address_space_memory (identity)\n");
    }

    qemu_log_mask(LOG_UNIMP,
                  "sweet-disp0: realized main=0x%" PRIx64 " aux=0x%" PRIx64
                  " pmgr-a=0x%" PRIx64 " pmgr-b=0x%" PRIx64
                  " fb=0x%" PRIx64 " %ux%u BGRA\n",
                  s->main_base, s->aux_base, s->pmgr_a_base, s->pmgr_b_base,
                  s->fb_base, s->fb_width, s->fb_height);
}

static void sweet_disp0_instance_init(Object *obj)
{
    SweetDisp0State *s = SWEET_DISP0(obj);

    s->bank_main.dev = s;
    s->bank_main.name = "main";
    s->bank_aux.dev = s;
    s->bank_aux.name = "aux";
    s->bank_pmgr_a.dev = s;
    s->bank_pmgr_a.name = "pmgr-a";
    s->bank_pmgr_b.dev = s;
    s->bank_pmgr_b.name = "pmgr-b";

    /* Reserved: not wired to any interrupt controller in stage 1. */
    sysbus_init_irq(SYS_BUS_DEVICE(obj), &s->irq);
}

static const Property sweet_disp0_properties[] = {
    DEFINE_PROP_LINK("dart", SweetDisp0State, dart_obj, TYPE_SWEET_DART,
                     Object *),
    DEFINE_PROP_UINT64("fb-base", SweetDisp0State, fb_base, 0x9c000000),
    DEFINE_PROP_UINT32("fb-width", SweetDisp0State, fb_width, 1080),
    DEFINE_PROP_UINT32("fb-height", SweetDisp0State, fb_height, 2400),
    DEFINE_PROP_UINT64("main-base", SweetDisp0State, main_base, 0x18000000),
    DEFINE_PROP_UINT64("aux-base", SweetDisp0State, aux_base, 0x18708000),
    DEFINE_PROP_UINT64("pmgr-a-base", SweetDisp0State, pmgr_a_base,
                       0x320b8000),
    DEFINE_PROP_UINT64("pmgr-b-base", SweetDisp0State, pmgr_b_base,
                       0x3230c000),
};

static void sweet_disp0_class_init(ObjectClass *oc, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(oc);

    dc->realize = sweet_disp0_realize;
    dc->user_creatable = true;
    device_class_set_props(dc, sweet_disp0_properties);
    dc->desc = "Apple T8015 display controller (disp0,t8015) stage-1 model";
    set_bit(DEVICE_CATEGORY_DISPLAY, dc->categories);
}

static const TypeInfo sweet_disp0_info = {
    .name = TYPE_SWEET_DISP0,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(SweetDisp0State),
    .instance_init = sweet_disp0_instance_init,
    .class_init = sweet_disp0_class_init,
};

/* ------------------------------------------------------------------ */
/* Host-driven swap hook (HMP `sweet-disp0-swap`)                      */
/* ------------------------------------------------------------------ */

/*
 * Resolve the sweet-disp0 instance from the QOM tree.  Stage 1 assumes a
 * single instance (the ios-sweet runner creates exactly one); if several
 * exist object_resolve_path_type() returns NULL and we say so.
 */
void hmp_sweet_disp0_swap(Monitor *mon, const QDict *qdict)
{
    const char *addr_str = qdict_get_str(qdict, "addr");
    hwaddr surface_pa = (hwaddr)strtoull(addr_str, NULL, 0);
    SweetDisp0State *s = (SweetDisp0State *)object_resolve_path_type(
        "", TYPE_SWEET_DISP0, NULL);
    dma_addr_t len;
    void *buf;
    MemTxResult r;

    if (!s) {
        monitor_printf(mon,
                       "sweet-disp0: no sweet-disp0 device instantiated "
                       "(or multiple)\n");
        return;
    }

    len = (dma_addr_t)s->fb_width * s->fb_height * 4; /* BGRA */
    buf = g_malloc(len);

    /*
     * Stage 1: read the surface through the DART address space (identity
     * translation today -- this is what proves the DART wiring), then
     * bounce it into guest RAM at fb-base.
     */
    r = dma_memory_read(s->dma_as, surface_pa, buf, len,
                        MEMTXATTRS_UNSPECIFIED);
    if (r != MEMTX_OK) {
        g_free(buf);
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-disp0: swap DMA fault reading surface "
                      "PA=0x%" HWADDR_PRIx " (memtx=%u); FB unchanged\n",
                      surface_pa, (unsigned)r);
        monitor_printf(mon,
                       "sweet-disp0: swap FAILED (DMA fault "
                       "PA=0x%" HWADDR_PRIx ")\n", surface_pa);
        return;
    }

    cpu_physical_memory_write(s->fb_base, buf, len);
    s->last_surface = surface_pa;

    qemu_log_mask(LOG_UNIMP,
                  "sweet-disp0: swap surface PA=0x%" HWADDR_PRIx
                  " -> FB 0x%" PRIx64 " (%u bytes, DART-translated)\n",
                  surface_pa, s->fb_base, (unsigned)len);
    monitor_printf(mon,
                   "sweet-disp0: swapped %u bytes PA=0x%" HWADDR_PRIx
                   " -> FB 0x%" PRIx64 "\n",
                   (unsigned)len, surface_pa, s->fb_base);
    g_free(buf);
}

static void sweet_disp0_register_types(void)
{
    type_register_static(&sweet_disp0_info);
}

type_init(sweet_disp0_register_types)
