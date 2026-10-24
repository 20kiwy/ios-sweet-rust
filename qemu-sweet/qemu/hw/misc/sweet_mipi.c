/*
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Synopsys MIPI-DSI host controller (mipi-dsim-1,synopsys) -- stage-2
 * "sweet" model for the ios-sweet-rust project.
 *
 * Stage-2 scope (W3 of docs/PLAN-FULL-BOOT.md -- DSI command FIFO acks
 * so a panel driver's command loop completes):
 *   * One MMIO window self-mapped at the D22AP address (default
 *     0x18400000, size 0x100000).
 *   * Curated constants so the real iOS driver
 *     (AppleSynopsysMIPIDSIController) sees a healthy controller:
 *       0x00: DWC DSI version  = 0x3133302A ("100*" DWC pattern)
 *       0x58: PHY status       = 0x0000000F (all lanes in stop state,
 *                               so PHY polling passes)
 *       0x60: INTSTATUS        = write-1-to-clear
 *       0x64: INTMASK          = store+echo
 *   * COMMAND FIFO ACKS (stage 2):
 *       0x48: GEN_HDR   -- write = send a DSI packet header (the packet
 *                          completes instantly: the FIFO drains, the
 *                          header is latched for read-back, INTSTATUS
 *                          bit 0 (CMD_DONE) is set)
 *       0x4c: GEN_PLD   -- write = payload FIFO word (latched; the
 *                          transfer completes instantly)
 *       0x50: CMD_PKT_STATUS -- read: GEN_CMD_EMPTY=1 | GEN_CMD_FULL=0
 *                          | GEN_PLD_W_EMPTY=1 | GEN_PLD_W_FULL=0
 *                          (0x5) -- the FIFO always accepts and drains,
 *                          which is what a driver polling the DWC status
 *                          bits needs to proceed.
 *   * Everything else: store+echo backing store; writes log one compact
 *     LOG_UNIMP line, reads are silent (they flood).
 *
 * The bit-accurate D-PHY / video-mode engine stays future work driven
 * by real hardware traces (no kernel ever probed these registers: W2).
 *
 * Observability contract: `-d int,guest_errors,unimp -D qemu.log`, all
 * traces via qemu_log_mask(LOG_UNIMP|LOG_GUEST_ERROR) with the
 * "sweet-mipi-dsim:" prefix.
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

#define SWEET_MIPI_REG_VERSION   0x00
#define SWEET_MIPI_REG_GEN_HDR   0x48
#define SWEET_MIPI_REG_GEN_PLD   0x4c
#define SWEET_MIPI_REG_CMD_PKT_STATUS 0x50
#define SWEET_MIPI_REG_PHY_STATUS 0x58
#define SWEET_MIPI_REG_INTSTATUS 0x60
#define SWEET_MIPI_REG_INTMASK   0x64

/* Curated constants. */
#define SWEET_MIPI_VERSION_VAL   0x3133302aull /* "100*" DWC pattern */
#define SWEET_MIPI_PHY_STATUS_VAL 0x0000000full /* all lanes stop-state idle */
/*
 * CMD_PKT_STATUS (stage 2: the FIFO always accepts and drains):
 *   bit 0 GEN_CMD_EMPTY   = 1
 *   bit 1 GEN_CMD_FULL    = 0
 *   bit 2 GEN_PLD_W_EMPTY = 1
 *   bit 3 GEN_PLD_W_FULL  = 0
 */
#define SWEET_MIPI_CMD_PKT_STATUS_VAL 0x00000005ull
/* INTSTATUS bit 0: a GEN_HDR packet completed. */
#define SWEET_MIPI_INT_CMD_DONE (1u << 0)

typedef struct SweetMipiState {
    SysBusDevice parent_obj;

    MemoryRegion mmio;
    qemu_irq irq;        /* reserved: never asserted in stage 1 */

    uint64_t base;       /* default 0x18400000 */
    uint32_t size;       /* default 0x100000 */

    uint32_t int_status; /* 0x60, W1C; bit0 CMD_DONE self-asserts */
    uint32_t int_mask;   /* 0x64, store+echo */

    /* Stage-2 command FIFO evidence (read-back of the last packet). */
    uint32_t last_hdr;
    uint32_t last_pld;

    uint8_t *store;      /* byte-granular store+echo backing (LE) */
} SweetMipiState;

DECLARE_INSTANCE_CHECKER(SweetMipiState, SWEET_MIPI, TYPE_SWEET_MIPI)

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

/* Reads silent. */
static uint64_t sweet_mipi_read(void *opaque, hwaddr addr, unsigned size)
{
    SweetMipiState *s = SWEET_MIPI(opaque);

    switch (addr) {
    case SWEET_MIPI_REG_VERSION:
        if (size == 4) {
            return SWEET_MIPI_VERSION_VAL;
        }
        break;
    case SWEET_MIPI_REG_GEN_HDR:
        if (size == 4) {
            return s->last_hdr;
        }
        break;
    case SWEET_MIPI_REG_GEN_PLD:
        if (size == 4) {
            return s->last_pld;
        }
        break;
    case SWEET_MIPI_REG_CMD_PKT_STATUS:
        if (size == 4) {
            return SWEET_MIPI_CMD_PKT_STATUS_VAL;
        }
        break;
    case SWEET_MIPI_REG_PHY_STATUS:
        if (size == 4) {
            return SWEET_MIPI_PHY_STATUS_VAL;
        }
        break;
    case SWEET_MIPI_REG_INTSTATUS:
        if (size == 4) {
            return s->int_status;
        }
        break;
    case SWEET_MIPI_REG_INTMASK:
        if (size == 4) {
            return s->int_mask;
        }
        break;
    default:
        break;
    }

    return sweet_store_read_le(s->store, addr, size);
}

static void sweet_mipi_write(void *opaque, hwaddr addr, uint64_t val,
                             unsigned size)
{
    SweetMipiState *s = SWEET_MIPI(opaque);

    switch (addr) {
    case SWEET_MIPI_REG_VERSION:
        /* Read-only version. */
        qemu_log_mask(LOG_UNIMP,
                      "sweet-mipi-dsim: W VERSION (read-only, ignored) = "
                      "0x%08" PRIx64 "\n", val);
        return;
    case SWEET_MIPI_REG_GEN_HDR:
        if (size == 4) {
            /* Stage 2: the packet completes instantly.  Latch the
             * header, raise INTSTATUS CMD_DONE, log the DSI packet. */
            s->last_hdr = (uint32_t)val;
            s->int_status |= SWEET_MIPI_INT_CMD_DONE;
            qemu_log_mask(LOG_UNIMP,
                          "sweet-mipi-dsim: DSI packet hdr=0x%08" PRIx64
                          " sent (FIFO acked, INTSTATUS CMD_DONE)\n", val);
            return;
        }
        break;
    case SWEET_MIPI_REG_GEN_PLD:
        if (size == 4) {
            s->last_pld = (uint32_t)val;
            qemu_log_mask(LOG_UNIMP,
                          "sweet-mipi-dsim: DSI payload word 0x%08" PRIx64
                          " accepted\n", val);
            return;
        }
        break;
    case SWEET_MIPI_REG_PHY_STATUS:
        sweet_store_write_le(s->store, addr, val, size);
        qemu_log_mask(LOG_UNIMP,
                      "sweet-mipi-dsim: W PHY_STATUS = 0x%08" PRIx64
                      " (stage-1 constant on read)\n", val);
        return;
    case SWEET_MIPI_REG_INTSTATUS:
        if (size == 4) {
            /* Write-1-to-clear (CMD_DONE included). */
            s->int_status &= ~(uint32_t)val;
            if (val != 0) {
                qemu_log_mask(LOG_UNIMP,
                              "sweet-mipi-dsim: W1C INTSTATUS = 0x%08"
                              PRIx64 "\n", val);
            }
            return;
        }
        break;
    case SWEET_MIPI_REG_INTMASK:
        if (size == 4) {
            s->int_mask = (uint32_t)val;
            sweet_store_write_le(s->store, addr, val, size);
            qemu_log_mask(LOG_UNIMP,
                          "sweet-mipi-dsim: W INTMASK = 0x%08" PRIx64 "\n",
                          val);
            return;
        }
        break;
    default:
        break;
    }

    /* Unknown offset: store+echo, one compact log line. */
    sweet_store_write_le(s->store, addr, val, size);
    qemu_log_mask(LOG_UNIMP,
                  "sweet-mipi-dsim: W unk[0x%04" HWADDR_PRIx "] = 0x%08"
                  PRIx64 " (store+echo)\n", addr, val);
}

static const MemoryRegionOps sweet_mipi_ops = {
    .read = sweet_mipi_read,
    .write = sweet_mipi_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .valid.min_access_size = 4,
    .valid.max_access_size = 4,
    .impl.min_access_size = 4,
    .impl.max_access_size = 4,
};

static void sweet_mipi_realize(DeviceState *dev, Error **errp)
{
    SweetMipiState *s = SWEET_MIPI(dev);

    if (s->size == 0) {
        error_setg(errp, "sweet-mipi-dsim: property size must be nonzero");
        return;
    }

    s->store = g_malloc0(s->size);

    memory_region_init_io(&s->mmio, OBJECT(s), &sweet_mipi_ops, s,
                          "sweet-mipi-dsim", s->size);

    /* Self-map (priority 1, see sweet_dart.c comment). */
    memory_region_add_subregion_overlap(get_system_memory(), s->base,
                                        &s->mmio, 1);

    qemu_log_mask(LOG_UNIMP,
                  "sweet-mipi-dsim: realized base=0x%" PRIx64 " size=0x%x"
                  " (stage-2: command FIFO acks + store+echo)\n", s->base,
                  s->size);
}

static void sweet_mipi_instance_init(Object *obj)
{
    SweetMipiState *s = SWEET_MIPI(obj);

    /* Reserved: not wired to any interrupt controller in stage 1. */
    sysbus_init_irq(SYS_BUS_DEVICE(obj), &s->irq);
}

static const Property sweet_mipi_properties[] = {
    DEFINE_PROP_UINT64("base", SweetMipiState, base, 0x18400000),
    DEFINE_PROP_UINT32("size", SweetMipiState, size, 0x100000),
};

static void sweet_mipi_class_init(ObjectClass *oc, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(oc);

    dc->realize = sweet_mipi_realize;
    dc->user_creatable = true;
    device_class_set_props(dc, sweet_mipi_properties);
    dc->desc = "Synopsys MIPI-DSI host (mipi-dsim-1,synopsys) stage-2 model";
    set_bit(DEVICE_CATEGORY_MISC, dc->categories);
}

static const TypeInfo sweet_mipi_info = {
    .name = TYPE_SWEET_MIPI,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(SweetMipiState),
    .instance_init = sweet_mipi_instance_init,
    .class_init = sweet_mipi_class_init,
};

static void sweet_mipi_register_types(void)
{
    type_register_static(&sweet_mipi_info);
}

type_init(sweet_mipi_register_types)
