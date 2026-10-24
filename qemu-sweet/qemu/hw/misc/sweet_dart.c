/*
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Apple T8015 DART (Device Address Resolution Table) -- stage-2 "sweet"
 * model for the ios-sweet-rust project.
 *
 * Stage 2 adds the real translation machinery on top of the stage-1
 * register model (W3 of docs/PLAN-FULL-BOOT.md, spec from the real
 * D22AP ADT node /arm-io/dart-disp0 -- docs/research/devicetree-d22ap.md
 * lines 1910-1939 -- plus public DART knowledge, marked RE-grade below):
 *
 *   * A REAL PTE walker: TTBR0[sid] -> 4-level walk of guest RAM with
 *     page-size = 0x1000 and stt-idx-width = 10 (both values are the
 *     ADT's own properties for this node).  Table entries are 64-bit
 *     little-endian words read straight from guest RAM.
 *
 *   * A translation cache (TLB) so repeated DMAs do not re-walk, with
 *     the old-DART-style explicit invalidate: STREAM_COMMAND bit 0
 *     (INVALIDATE_ALL) flushes it.  TCR/TTBR writes flush it too
 *     (safety net for a model without real TLB coherency -- documented
 *     deviation, see the walk contract below).
 *
 *   * REAL fault reporting: FAULT_STATUS / FAULT_IOVA_LO / FAULT_IOVA_HI
 *     latch the last denied DMA, the fault IRQ line (stage-1 reserved)
 *     is asserted on the first fault and deasserted when FAULT_STATUS
 *     is acknowledged (write-1-to-clear).  Denied DMAs stay denied
 *     (IOMMU_NONE) -- the stage-1 deny path, now with registers.
 *
 *   * Register layout fix over stage 1: TTBR0[sid][n] moves from
 *     0x200 + sid*0x100 to 0x300 + sid*0x100 + 8*n.  Stage 1 placed
 *     TTBR0[0][0] at 0x200 which is ALSO TCR[2] (0x000 + 2*0x100), and
 *     the TCR matcher runs first -- with the D22AP's sids=3 the first
 *     TTBR slot was unreachable.  Never hit before stage 2 because no
 *     guest programs the DART (W2 verdict), which is exactly why the
 *     collision survived.
 *
 * Walk contract (RE-grade, frozen for this model -- the stage-1 traces
 * never materialized because W2 closed; the ADT parameters + the
 * M1-DART-style PTE bit layout below are the spec):
 *
 *   IOVA (up to 40 bits; the IOMMU window stays 4 GiB so every DMA in
 *   this machine has IOVA < 4 GiB):
 *       [11:0]  page offset           (page-size = 0x1000)
 *       [20:12] L3 index (9 bits)
 *       [29:21] L2 index (9 bits)
 *       [39:30] L1/"STT" index (10 bits -- stt-idx-width = 10)
 *   For IOVA < 4 GiB the L1 index is always 0: one L1 table covers the
 *   whole DMA window through L1[0] -> L2 -> L3, which is what the real
 *   display DART does for surfaces below 4 GiB as well.
 *
 *   TTBR0[sid][n] (n = 0..3):
 *       bit 0        VALID -- the table is live
 *       bits [47:12] physical base of the L1 ("STT") table, 4 KB aligned
 *   The TTBR slot index n is chosen per transaction by the requester's
 *   sub-TTBR selector; stage 2 carries no requester attributes (QEMU
 *   iommu_idx is always 0 here), so DMA uses slot 0.  All four slots
 *   are programmed/readable through MMIO and exercised by the W3 gate.
 *
 *   Table entries and leaf PTEs (64-bit LE, M1-DART-style layout):
 *       bit 0        VALID
 *       bit 1        PROT_NO_READ  (1 = reads fault)
 *       bit 2        PROT_NO_WRITE (1 = writes fault)
 *       bits [47:12] next-table base (intermediate levels) or
 *                    output PA >> 12 (leaf)
 *
 *   TCR bits (unchanged from stage 1):
 *       bit 0 TRANSLATE_ENABLE -- when clear, DMA passes through.
 *       bit 1 BYPASS           -- when set, DMA passes through even if
 *                                 translation is enabled.
 *
 * Observability contract (unchanged): the Rust runner boots with
 * `-d int,guest_errors,unimp -D qemu.log`, so ALL logging goes through
 * qemu_log_mask(LOG_UNIMP|LOG_GUEST_ERROR) with the "sweet-dart:" prefix.
 * Walks are logged on TLB miss only (flood control); cache hits are
 * silent.
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
#include "system/dma.h"

/* ------------------------------------------------------------------ */
/* Register map (stage-2, see the walk contract above)                 */
/* ------------------------------------------------------------------ */

/* Per-SID register block stride. */
#define SWEET_DART_SID_STRIDE   0x100ull
#define SWEET_DART_TCR_OFF(sid)   (0x000ull + SWEET_DART_SID_STRIDE * (sid))
/*
 * Stage-2 TTBR base: 0x300 + sid*0x100 + 8*n.  (Stage 1 used 0x200,
 * which collided with TCR[2] -- see the header comment.)
 */
#define SWEET_DART_TTBR_BASE    0x300ull
#define SWEET_DART_TTBR0_OFF(sid, n) \
    (SWEET_DART_TTBR_BASE + SWEET_DART_SID_STRIDE * (sid) + 8ull * (n))

/* Global registers (below the first per-SID block). */
#define SWEET_DART_REG_STREAM_COMMAND 0x020
#define SWEET_DART_REG_STREAM_SELECT  0x034
#define SWEET_DART_REG_FAULT_STATUS   0x040
#define SWEET_DART_REG_FAULT_IOVA_LO  0x044
#define SWEET_DART_REG_FAULT_IOVA_HI  0x048

/* STREAM_COMMAND bits (write-1-to-execute; reads as 0 -- instant). */
#define SWEET_DART_SCMD_INVALIDATE_ALL (1ull << 0)

/* FAULT_STATUS bits. */
#define SWEET_DART_FAULT_VALID   (1ull << 0)
#define SWEET_DART_FAULT_SID_SHIFT 8
#define SWEET_DART_FAULT_SID_MASK  0xffull

/*
 * TCR bits (stage-1 semantics, now actually consumed by the walker):
 *   bit 0: TRANSLATE_ENABLE -- when clear, DMA passes through untouched.
 *   bit 1: BYPASS          -- when set, DMA passes through even if
 *                             translation is enabled.
 */
#define SWEET_DART_TCR_TRANSLATE_ENABLE (1ull << 0)
#define SWEET_DART_TCR_BYPASS           (1ull << 1)
/*
 * Reset value: bypass on, translate off.  Mirrors a DART left in bypass
 * by iBoot, so DMA works out of the box on stage 1.
 */
#define SWEET_DART_TCR_RESET            SWEET_DART_TCR_BYPASS

/* PTE bits (walk contract above). */
#define SWEET_DART_PTE_VALID       (1ull << 0)
#define SWEET_DART_PTE_NO_READ     (1ull << 1)
#define SWEET_DART_PTE_NO_WRITE    (1ull << 2)
#define SWEET_DART_PTE_ADDR_MASK   0xfffffffff000ull
#define SWEET_DART_PTE_ADDR_SHIFT  12

/* Walk geometry (from the real D22AP ADT node). */
#define SWEET_DART_PAGE_SIZE       0x1000
#define SWEET_DART_PAGE_SHIFT      12
#define SWEET_DART_STT_IDX_WIDTH   10   /* ADT stt-idx-width = 0xa */
#define SWEET_DART_L2_IDX_WIDTH    9
#define SWEET_DART_L3_IDX_WIDTH    9

#define SWEET_DART_MAX_SIDS    16
#define SWEET_DART_TTBR_SLOTS  4

/* Translation cache: direct-mapped, invalidated by STREAM_COMMAND.
 * 4096 entries cover the whole 10 MB disp0 surface (2530 pages) without
 * evictions, so the W3 gate's stale-entry check is deterministic. */
#define SWEET_DART_TLB_ENTRIES 4096

typedef struct SweetDartTlbEntry {
    uint64_t iova_page;  /* iova >> 12 */
    uint32_t sid;
    bool valid;
    hwaddr pa_page;      /* translated_addr >> 12 */
    uint32_t perm;       /* IOMMU_RW / IOMMU_NONE after a faulting walk */
} SweetDartTlbEntry;

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

    /* Fault IRQ line: asserted on the first denied DMA, deasserted when
     * FAULT_STATUS is W1C-acked.  Harmless when nobody wires it. */
    qemu_irq irq;
    bool irq_level;

    /* Properties. */
    uint64_t base;        /* main window base  (default 0x18704000) */
    uint64_t remap_base;  /* remap window base (default 0x18700000) */
    uint32_t size;        /* window size        (default 0x4000)    */
    uint32_t sids;        /* stream IDs         (default 3, max 16) */

    /* Stage-2 translation state.  The DMA requester SID is not carried
     * by QEMU here (iommu_idx == 0); translation uses the global /
     * SID 0 view.  The per-SID banks stay programmable for fidelity
     * and the W3 gate. */
    uint64_t *tcr;        /* sids entries */
    uint64_t *ttbr0;      /* sids * SWEET_DART_TTBR_SLOTS entries */

    /* Latched fault state (FAULT_STATUS / FAULT_IOVA_*). */
    uint32_t fault_status;
    uint64_t fault_iova;
    uint32_t fault_sid;

    /* Translation cache (TLB). */
    SweetDartTlbEntry *tlb;

    /* Byte-granular store+echo backing arrays (LE layout). */
    uint8_t *store;
    uint8_t *store_remap;
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
/* Translation cache                                                   */
/* ------------------------------------------------------------------ */

static inline uint32_t dart_tlb_hash(uint32_t sid, uint64_t iova_page)
{
    /* Linear in the page number for SID 0 (a surface walk touches
     * consecutive entries with no collisions); other SIDs are offset so
     * they cannot silently alias SID 0's entries (aliasing only costs
     * evictions -- the entry carries sid for validation). */
    return (uint32_t)((iova_page + ((uint64_t)sid << 24)) &
                      (SWEET_DART_TLB_ENTRIES - 1));
}

static void dart_tlb_flush(SweetDartState *s, const char *why)
{
    memset(s->tlb, 0, SWEET_DART_TLB_ENTRIES * sizeof(SweetDartTlbEntry));
    qemu_log_mask(LOG_UNIMP, "sweet-dart: TLB invalidated (%s)\n", why);
}

/* ------------------------------------------------------------------ */
/* The stage-2 PTE walker                                              */
/* ------------------------------------------------------------------ */

/* Read one 64-bit table entry from guest RAM.  Returns false on a
 * DMA error (table PA outside RAM -- reported as a walk fault). */
static bool dart_read_entry(SweetDartState *s, uint64_t table_pa,
                            uint32_t index, uint64_t *entry)
{
    uint64_t addr = (table_pa & SWEET_DART_PTE_ADDR_MASK)
                    + (uint64_t)index * 8;
    MemTxResult r;

    r = dma_memory_read(&address_space_memory, addr, entry, 8,
                        MEMTXATTRS_UNSPECIFIED);
    if (r != MEMTX_OK) {
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-dart: walk fault: table read failed "
                      "pa=0x%016" PRIx64 " (memtx=%u)\n", addr,
                      (unsigned)r);
        return false;
    }
    return true;
}

/* One intermediate level: read entry[index], require VALID, return the
 * next-table base.  Returns false and logs the fault otherwise. */
static bool dart_walk_level(SweetDartState *s, const char *name,
                            uint64_t table_pa, uint32_t index,
                            uint64_t iova, uint64_t *next_base)
{
    uint64_t entry;

    if (!dart_read_entry(s, table_pa, index, &entry)) {
        return false;
    }
    if (!(entry & SWEET_DART_PTE_VALID)) {
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-dart: DMA fault iova=0x%" HWADDR_PRIx
                      " (%s[%u] not valid)\n", iova, name, index);
        return false;
    }
    *next_base = entry & SWEET_DART_PTE_ADDR_MASK;
    return true;
}

/*
 * The full walk.  On success sets *pa_page / *perm and returns true.
 * On any fault: latch FAULT_STATUS / FAULT_IOVA / FAULT_SID, raise the
 * fault IRQ, set *perm = IOMMU_NONE and return false.
 */
static bool dart_walk(SweetDartState *s, uint32_t sid, hwaddr iova,
                      IOMMUAccessFlags flag, hwaddr *pa_page,
                      uint32_t *perm)
{
    uint64_t ttbr = s->ttbr0[sid * SWEET_DART_TTBR_SLOTS + 0];
    uint64_t l1_base, l2_base, l3_base, pte;
    uint32_t idx_l1, idx_l2, idx_l3;

    /* TTBR slot 0 must be live (walk contract: DMA uses slot 0). */
    if (!(ttbr & SWEET_DART_PTE_VALID) ||
        (ttbr & SWEET_DART_PTE_ADDR_MASK) == 0) {
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-dart: DMA fault iova=0x%" HWADDR_PRIx
                      " (TTBR0[%u][0]=0x%016" PRIx64
                      ": unconfigured, translation on)\n",
                      iova, sid, ttbr);
        goto fault;
    }

    idx_l1 = (uint32_t)((iova >> (SWEET_DART_PAGE_SHIFT
                                  + SWEET_DART_L3_IDX_WIDTH
                                  + SWEET_DART_L2_IDX_WIDTH))
                        & ((1u << SWEET_DART_STT_IDX_WIDTH) - 1));
    idx_l2 = (uint32_t)((iova >> (SWEET_DART_PAGE_SHIFT
                                  + SWEET_DART_L3_IDX_WIDTH))
                        & ((1u << SWEET_DART_L2_IDX_WIDTH) - 1));
    idx_l3 = (uint32_t)((iova >> SWEET_DART_PAGE_SHIFT)
                        & ((1u << SWEET_DART_L3_IDX_WIDTH) - 1));

    if (!dart_walk_level(s, "L1", ttbr, idx_l1, iova, &l2_base)) {
        goto fault;
    }
    if (!dart_walk_level(s, "L2", l2_base, idx_l2, iova, &l3_base)) {
        goto fault;
    }
    if (!dart_walk_level(s, "L3", l3_base, idx_l3, iova, &pte)) {
        goto fault;
    }

    /* Leaf permission check (M1-DART-style NO_READ/NO_WRITE bits). */
    if ((flag & IOMMU_RO) && (pte & SWEET_DART_PTE_NO_READ)) {
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-dart: DMA fault iova=0x%" HWADDR_PRIx
                      " (PTE NO_READ)\n", iova);
        goto fault;
    }
    if ((flag & IOMMU_WO) && (pte & SWEET_DART_PTE_NO_WRITE)) {
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-dart: DMA fault iova=0x%" HWADDR_PRIx
                      " (PTE NO_WRITE)\n", iova);
        goto fault;
    }

    *pa_page = (pte & SWEET_DART_PTE_ADDR_MASK) >> SWEET_DART_PTE_ADDR_SHIFT;
    *perm = IOMMU_RW;

    qemu_log_mask(LOG_UNIMP,
                  "sweet-dart: walk iova=0x%016" HWADDR_PRIx
                  " -> pa=0x%016" HWADDR_PRIx
                  " (L1[%u] L2[%u] L3[%u], sid %u)\n",
                  iova, *pa_page << SWEET_DART_PAGE_SHIFT,
                  idx_l1, idx_l2, idx_l3, sid);
    return true;

fault:
    s->fault_status = (uint32_t)SWEET_DART_FAULT_VALID
                      | (uint32_t)((sid & SWEET_DART_FAULT_SID_MASK)
                                   << SWEET_DART_FAULT_SID_SHIFT);
    s->fault_iova = iova;
    s->fault_sid = sid;
    *perm = IOMMU_NONE;
    if (!s->irq_level) {
        s->irq_level = true;
        qemu_set_irq(s->irq, 1);
        qemu_log_mask(LOG_GUEST_ERROR,
                      "sweet-dart: fault IRQ asserted (sid %u)\n", sid);
    }
    return false;
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

/* Reads are intentionally silent for the store+echo bulk: TCR/TTBR
 * values live in the same backing store, so kernel read-back checks
 * pass.  The curated registers below answer from live state. */
static uint64_t sweet_dart_mmio_read(void *opaque, hwaddr addr, unsigned size)
{
    SweetDartState *s = SWEET_DART(opaque);

    switch (addr) {
    case SWEET_DART_REG_STREAM_COMMAND:
        /* Self-clearing command register: always reads 0. */
        if (size == 4) {
            return 0;
        }
        break;
    case SWEET_DART_REG_FAULT_STATUS:
        if (size == 4) {
            return s->fault_status;
        }
        break;
    case SWEET_DART_REG_FAULT_IOVA_LO:
        if (size == 4) {
            return (uint32_t)s->fault_iova;
        }
        break;
    case SWEET_DART_REG_FAULT_IOVA_HI:
        if (size == 4) {
            return (uint32_t)(s->fault_iova >> 32);
        }
        break;
    default:
        break;
    }

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
                          "stage-2 PTE walker live (program TTBR0, then "
                          "STREAM_COMMAND invalidate)\n", sid);
        }
        /* Translation state changed: drop cached translations. */
        if (old != s->tcr[sid]) {
            dart_tlb_flush(s, "TCR write");
        }
        return;
    }

    if (dart_ttbr_hit(s, addr, size, &sid, &slot)) {
        s->ttbr0[sid * SWEET_DART_TTBR_SLOTS + slot] = val;
        sweet_store_write_le(s->store, addr, val, size);
        qemu_log_mask(LOG_UNIMP,
                      "sweet-dart: W TTBR0[%u][%u] = 0x%016" PRIx64
                      "%s%s\n", sid, slot, val,
                      (val & SWEET_DART_PTE_VALID) ? " [VALID]" : "",
                      ((val & SWEET_DART_PTE_ADDR_MASK)
                       && (val & SWEET_DART_PTE_VALID))
                          ? " (L1 table live)" : "");
        /* New table pointer: cached walks for this DART are stale. */
        dart_tlb_flush(s, "TTBR write");
        return;
    }

    switch (addr) {
    case SWEET_DART_REG_STREAM_COMMAND:
        if (size == 4) {
            if (val & SWEET_DART_SCMD_INVALIDATE_ALL) {
                dart_tlb_flush(s, "STREAM_COMMAND invalidate");
            }
            if (val & ~SWEET_DART_SCMD_INVALIDATE_ALL) {
                qemu_log_mask(LOG_UNIMP,
                              "sweet-dart: W STREAM_COMMAND reserved bits "
                              "0x%08" PRIx64 " (ignored)\n",
                              val & ~SWEET_DART_SCMD_INVALIDATE_ALL);
            }
            return;
        }
        break;
    case SWEET_DART_REG_STREAM_SELECT:
        if (size == 4) {
            sweet_store_write_le(s->store, addr, val, size);
            qemu_log_mask(LOG_UNIMP,
                          "sweet-dart: W STREAM_SELECT = 0x%08" PRIx64
                          " (invalidate scope: model flushes all)\n", val);
            return;
        }
        break;
    case SWEET_DART_REG_FAULT_STATUS:
        if (size == 4) {
            /* Write-1-to-clear: acking VALID deasserts the fault IRQ. */
            s->fault_status &= ~(uint32_t)val;
            if (val & SWEET_DART_FAULT_VALID) {
                qemu_log_mask(LOG_UNIMP,
                              "sweet-dart: FAULT_STATUS acked (iova was "
                              "0x%016" HWADDR_PRIx ", sid %u)\n",
                              s->fault_iova, s->fault_sid);
                if (s->irq_level) {
                    s->irq_level = false;
                    qemu_set_irq(s->irq, 0);
                    qemu_log_mask(LOG_UNIMP,
                                  "sweet-dart: fault IRQ deasserted\n");
                }
            }
            return;
        }
        break;
    default:
        break;
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
/* IOMMU (stage 2: real walk, TLB-cached; bypass/pass-through unless   */
/* TRANSLATE_ENABLE is set without BYPASS)                             */
/* ------------------------------------------------------------------ */

static IOMMUTLBEntry sweet_dart_iommu_translate(IOMMUMemoryRegion *iommu,
                                                hwaddr addr,
                                                IOMMUAccessFlags flag,
                                                int iommu_idx)
{
    SweetDartState *s = container_of(iommu, SweetDartState, iommu);
    /* The DMA requester SID is not carried by QEMU here; use the global
     * (SID 0) view.  iommu_idx is unused (single AS index). */
    const uint32_t sid = 0;
    uint64_t tcr = s->tcr[sid];
    IOMMUTLBEntry ret = {
        .target_as = &address_space_memory,
        .iova = addr & ~0xfffull,
        .translated_addr = 0,
        .addr_mask = 0xfff,
        .perm = IOMMU_NONE,
    };
    uint32_t tlb_idx;
    SweetDartTlbEntry *e;
    hwaddr pa_page;
    uint32_t perm;
    bool translating;

    (void)iommu_idx;

    translating = (tcr & SWEET_DART_TCR_TRANSLATE_ENABLE) &&
                  !(tcr & SWEET_DART_TCR_BYPASS);
    if (!translating) {
        /* Bypass enabled or translation disabled: identity. */
        ret.translated_addr = addr & ~0xfffull;
        ret.perm = IOMMU_RW;
        return ret;
    }

    /* Translation cache lookup. */
    tlb_idx = dart_tlb_hash(sid, addr >> SWEET_DART_PAGE_SHIFT);
    e = &s->tlb[tlb_idx];
    if (e->valid && e->sid == sid &&
        e->iova_page == (addr >> SWEET_DART_PAGE_SHIFT)) {
        /* Hit: a cached fault stays a fault, a cached translation is
         * served without re-walking. */
        ret.perm = e->perm;
        if (e->perm != IOMMU_NONE) {
            ret.translated_addr = e->pa_page << SWEET_DART_PAGE_SHIFT;
        }
        return ret;
    }

    /* Miss: the real walk. */
    if (dart_walk(s, sid, addr, flag, &pa_page, &perm)) {
        e->valid = true;
        e->sid = sid;
        e->iova_page = addr >> SWEET_DART_PAGE_SHIFT;
        e->pa_page = pa_page;
        e->perm = IOMMU_RW;
        ret.perm = IOMMU_RW;
        ret.translated_addr = pa_page << SWEET_DART_PAGE_SHIFT;
    } else {
        /* Cache the fault too, so a broken guest does not re-walk
         * forever; the TLB is flushed by STREAM_COMMAND / TCR / TTBR. */
        e->valid = true;
        e->sid = sid;
        e->iova_page = addr >> SWEET_DART_PAGE_SHIFT;
        e->pa_page = 0;
        e->perm = IOMMU_NONE;
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
    s->tlb = g_new0(SweetDartTlbEntry, SWEET_DART_TLB_ENTRIES);

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
                  " size=0x%x sids=%u (stage-2 PTE walker: page 0x%x, "
                  "stt-idx-width %u, TTBR @ 0x%03llx)\n",
                  s->base, s->remap_base, s->size, s->sids,
                  SWEET_DART_PAGE_SIZE, SWEET_DART_STT_IDX_WIDTH,
                  (unsigned long long)SWEET_DART_TTBR_BASE);
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
    s->fault_status = 0;
    s->fault_iova = 0;
    s->fault_sid = 0;
    if (s->irq_level) {
        s->irq_level = false;
        qemu_set_irq(s->irq, 0);
    }
    if (s->tlb) {
        memset(s->tlb, 0, SWEET_DART_TLB_ENTRIES * sizeof(SweetDartTlbEntry));
    }
}

static void sweet_dart_instance_init(Object *obj)
{
    SweetDartState *s = SWEET_DART(obj);

    /* Fault IRQ: asserted by real faults in stage 2. */
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
    dc->desc = "Apple T8015 DART (dart,t8015) stage-2 model (PTE walker)";
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
