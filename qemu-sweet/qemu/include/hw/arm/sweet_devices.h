/*
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Apple T8015 "sweet" device models for the ios-sweet-rust project.
 *
 * Stage 1 scope: register-level store+echo models with curated constants
 * for the D22AP (iPhone X) display path, so the real iOS 16 XNU kernel
 * drivers (AppleT8015DART, AppleSynopsysMIPIDSIController, disp0 subsystem)
 * can probe real MMIO addresses on QEMU 'virt'.
 *
 * All devices are user-creatable (-device) and self-map their MMIO regions
 * into the system address space at the D22AP physical addresses using
 * memory_region_add_subregion_overlap(get_system_memory(), base, mr, 1)
 * (priority 1 wins over the empty pcie-mmio alias container on 'virt'),
 * so no hw/arm/virt.c changes are required.
 */
#ifndef HW_ARM_SWEET_DEVICES_H
#define HW_ARM_SWEET_DEVICES_H

#include "exec/memory.h"
#include "qom/object.h"

#define TYPE_SWEET_DART  "sweet-dart"
#define TYPE_SWEET_DISP0 "sweet-disp0"
#define TYPE_SWEET_MIPI  "sweet-mipi-dsim"

/*
 * IOMMUMemoryRegion subclass instantiated inside the DART state.  It is
 * never mapped into the system address space; it is only the root of the
 * DMA AddressSpace handed to sweet-disp0 (and later to other bus masters).
 */
#define TYPE_SWEET_DART_IOMMU_MEMORY_REGION "sweet-dart-iommu-memory-region"

typedef struct SweetDartState SweetDartState;

/*
 * Return the DART's IOMMU MemoryRegion (4 GiB IOVA window).  Used by
 * sweet_disp0.c to build its DMA AddressSpace:
 *   address_space_init(&as, sweet_dart_iommu_mr(dart_obj), "name");
 */
MemoryRegion *sweet_dart_iommu_mr(Object *dart_obj);

/*
 * HMP glue for the host-driven framebuffer swap hook (defined in
 * sweet_disp0.c, registered in hmp-commands.hx).  The prototype lives in
 * include/monitor/hmp.h as well, because that is what monitor/hmp-target.c
 * includes when it resolves hmp-commands.hx handler names.
 */
struct Monitor;
struct QDict;
void hmp_sweet_disp0_swap(struct Monitor *mon, const struct QDict *qdict);

#endif /* HW_ARM_SWEET_DEVICES_H */
