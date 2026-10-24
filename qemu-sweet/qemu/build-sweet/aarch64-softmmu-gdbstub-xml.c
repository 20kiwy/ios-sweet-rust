#include "qemu/osdep.h"
#include "exec/gdbstub.h"

const GDBFeature gdb_static_features[] = {
    {
        "aarch64-core.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2009-2012 Free Software Foundation, Inc.\n"
        "     Contributed by ARM Ltd.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.aarch64.core\">\n"
        "  <reg name=\"x0\" bitsize=\"64\"/>\n"
        "  <reg name=\"x1\" bitsize=\"64\"/>\n"
        "  <reg name=\"x2\" bitsize=\"64\"/>\n"
        "  <reg name=\"x3\" bitsize=\"64\"/>\n"
        "  <reg name=\"x4\" bitsize=\"64\"/>\n"
        "  <reg name=\"x5\" bitsize=\"64\"/>\n"
        "  <reg name=\"x6\" bitsize=\"64\"/>\n"
        "  <reg name=\"x7\" bitsize=\"64\"/>\n"
        "  <reg name=\"x8\" bitsize=\"64\"/>\n"
        "  <reg name=\"x9\" bitsize=\"64\"/>\n"
        "  <reg name=\"x10\" bitsize=\"64\"/>\n"
        "  <reg name=\"x11\" bitsize=\"64\"/>\n"
        "  <reg name=\"x12\" bitsize=\"64\"/>\n"
        "  <reg name=\"x13\" bitsize=\"64\"/>\n"
        "  <reg name=\"x14\" bitsize=\"64\"/>\n"
        "  <reg name=\"x15\" bitsize=\"64\"/>\n"
        "  <reg name=\"x16\" bitsize=\"64\"/>\n"
        "  <reg name=\"x17\" bitsize=\"64\"/>\n"
        "  <reg name=\"x18\" bitsize=\"64\"/>\n"
        "  <reg name=\"x19\" bitsize=\"64\"/>\n"
        "  <reg name=\"x20\" bitsize=\"64\"/>\n"
        "  <reg name=\"x21\" bitsize=\"64\"/>\n"
        "  <reg name=\"x22\" bitsize=\"64\"/>\n"
        "  <reg name=\"x23\" bitsize=\"64\"/>\n"
        "  <reg name=\"x24\" bitsize=\"64\"/>\n"
        "  <reg name=\"x25\" bitsize=\"64\"/>\n"
        "  <reg name=\"x26\" bitsize=\"64\"/>\n"
        "  <reg name=\"x27\" bitsize=\"64\"/>\n"
        "  <reg name=\"x28\" bitsize=\"64\"/>\n"
        "  <reg name=\"x29\" bitsize=\"64\"/>\n"
        "  <reg name=\"x30\" bitsize=\"64\"/>\n"
        "  <reg name=\"sp\" bitsize=\"64\" type=\"data_ptr\"/>\n"
        "\n"
        "  <reg name=\"pc\" bitsize=\"64\" type=\"code_ptr\"/>\n"
        "  <reg name=\"cpsr\" bitsize=\"32\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.aarch64.core",
        (const char * const []) {
            [0] =
                "x0",
            [1] =
                "x1",
            [2] =
                "x2",
            [3] =
                "x3",
            [4] =
                "x4",
            [5] =
                "x5",
            [6] =
                "x6",
            [7] =
                "x7",
            [8] =
                "x8",
            [9] =
                "x9",
            [10] =
                "x10",
            [11] =
                "x11",
            [12] =
                "x12",
            [13] =
                "x13",
            [14] =
                "x14",
            [15] =
                "x15",
            [16] =
                "x16",
            [17] =
                "x17",
            [18] =
                "x18",
            [19] =
                "x19",
            [20] =
                "x20",
            [21] =
                "x21",
            [22] =
                "x22",
            [23] =
                "x23",
            [24] =
                "x24",
            [25] =
                "x25",
            [26] =
                "x26",
            [27] =
                "x27",
            [28] =
                "x28",
            [29] =
                "x29",
            [30] =
                "x30",
            [31] =
                "sp",
            [32] =
                "pc",
            [33] =
                "cpsr",
        },
        34,
    },
    {
        "aarch64-fpu.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2009-2012 Free Software Foundation, Inc.\n"
        "     Contributed by ARM Ltd.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.aarch64.fpu\">\n"
        "  <vector id=\"v2d\" type=\"ieee_double\" count=\"2\"/>\n"
        "  <vector id=\"v2u\" type=\"uint64\" count=\"2\"/>\n"
        "  <vector id=\"v2i\" type=\"int64\" count=\"2\"/>\n"
        "  <vector id=\"v4f\" type=\"ieee_single\" count=\"4\"/>\n"
        "  <vector id=\"v4u\" type=\"uint32\" count=\"4\"/>\n"
        "  <vector id=\"v4i\" type=\"int32\" count=\"4\"/>\n"
        "  <vector id=\"v8u\" type=\"uint16\" count=\"8\"/>\n"
        "  <vector id=\"v8i\" type=\"int16\" count=\"8\"/>\n"
        "  <vector id=\"v16u\" type=\"uint8\" count=\"16\"/>\n"
        "  <vector id=\"v16i\" type=\"int8\" count=\"16\"/>\n"
        "  <vector id=\"v1u\" type=\"uint128\" count=\"1\"/>\n"
        "  <vector id=\"v1i\" type=\"int128\" count=\"1\"/>\n"
        "  <union id=\"vnd\">\n"
        "    <field name=\"f\" type=\"v2d\"/>\n"
        "    <field name=\"u\" type=\"v2u\"/>\n"
        "    <field name=\"s\" type=\"v2i\"/>\n"
        "  </union>\n"
        "  <union id=\"vns\">\n"
        "    <field name=\"f\" type=\"v4f\"/>\n"
        "    <field name=\"u\" type=\"v4u\"/>\n"
        "    <field name=\"s\" type=\"v4i\"/>\n"
        "  </union>\n"
        "  <union id=\"vnh\">\n"
        "    <field name=\"u\" type=\"v8u\"/>\n"
        "    <field name=\"s\" type=\"v8i\"/>\n"
        "  </union>\n"
        "  <union id=\"vnb\">\n"
        "    <field name=\"u\" type=\"v16u\"/>\n"
        "    <field name=\"s\" type=\"v16i\"/>\n"
        "  </union>\n"
        "  <union id=\"vnq\">\n"
        "    <field name=\"u\" type=\"v1u\"/>\n"
        "    <field name=\"s\" type=\"v1i\"/>\n"
        "  </union>\n"
        "  <union id=\"aarch64v\">\n"
        "    <field name=\"d\" type=\"vnd\"/>\n"
        "    <field name=\"s\" type=\"vns\"/>\n"
        "    <field name=\"h\" type=\"vnh\"/>\n"
        "    <field name=\"b\" type=\"vnb\"/>\n"
        "    <field name=\"q\" type=\"vnq\"/>\n"
        "  </union>\n"
        "  <reg name=\"v0\" bitsize=\"128\" type=\"aarch64v\" regnum=\"34\"/>\n"
        "  <reg name=\"v1\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v2\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v3\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v4\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v5\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v6\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v7\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v8\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v9\" bitsize=\"128\" type=\"aarch64v\" />\n"
        "  <reg name=\"v10\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v11\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v12\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v13\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v14\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v15\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v16\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v17\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v18\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v19\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v20\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v21\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v22\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v23\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v24\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v25\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v26\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v27\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v28\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v29\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v30\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"v31\" bitsize=\"128\" type=\"aarch64v\"/>\n"
        "  <reg name=\"fpsr\" bitsize=\"32\"/>\n"
        "  <reg name=\"fpcr\" bitsize=\"32\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.aarch64.fpu",
        (const char * const []) {
            [0] =
                "v0",
            [1] =
                "v1",
            [2] =
                "v2",
            [3] =
                "v3",
            [4] =
                "v4",
            [5] =
                "v5",
            [6] =
                "v6",
            [7] =
                "v7",
            [8] =
                "v8",
            [9] =
                "v9",
            [10] =
                "v10",
            [11] =
                "v11",
            [12] =
                "v12",
            [13] =
                "v13",
            [14] =
                "v14",
            [15] =
                "v15",
            [16] =
                "v16",
            [17] =
                "v17",
            [18] =
                "v18",
            [19] =
                "v19",
            [20] =
                "v20",
            [21] =
                "v21",
            [22] =
                "v22",
            [23] =
                "v23",
            [24] =
                "v24",
            [25] =
                "v25",
            [26] =
                "v26",
            [27] =
                "v27",
            [28] =
                "v28",
            [29] =
                "v29",
            [30] =
                "v30",
            [31] =
                "v31",
            [32] =
                "fpsr",
            [33] =
                "fpcr",
        },
        34,
    },
    {
        "arm-core.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2008 Free Software Foundation, Inc.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.arm.core\">\n"
        "  <reg name=\"r0\" bitsize=\"32\"/>\n"
        "  <reg name=\"r1\" bitsize=\"32\"/>\n"
        "  <reg name=\"r2\" bitsize=\"32\"/>\n"
        "  <reg name=\"r3\" bitsize=\"32\"/>\n"
        "  <reg name=\"r4\" bitsize=\"32\"/>\n"
        "  <reg name=\"r5\" bitsize=\"32\"/>\n"
        "  <reg name=\"r6\" bitsize=\"32\"/>\n"
        "  <reg name=\"r7\" bitsize=\"32\"/>\n"
        "  <reg name=\"r8\" bitsize=\"32\"/>\n"
        "  <reg name=\"r9\" bitsize=\"32\"/>\n"
        "  <reg name=\"r10\" bitsize=\"32\"/>\n"
        "  <reg name=\"r11\" bitsize=\"32\"/>\n"
        "  <reg name=\"r12\" bitsize=\"32\"/>\n"
        "  <reg name=\"sp\" bitsize=\"32\" type=\"data_ptr\"/>\n"
        "  <reg name=\"lr\" bitsize=\"32\"/>\n"
        "  <reg name=\"pc\" bitsize=\"32\" type=\"code_ptr\"/>\n"
        "\n"
        "  <!-- The CPSR is register 25, rather than register 16, because\n"
        "       the FPA registers historically were placed between the PC\n"
        "       and the CPSR in the \"g\" packet.  -->\n"
        "  <reg name=\"cpsr\" bitsize=\"32\" regnum=\"25\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.arm.core",
        (const char * const []) {
            [0] =
                "r0",
            [1] =
                "r1",
            [2] =
                "r2",
            [3] =
                "r3",
            [4] =
                "r4",
            [5] =
                "r5",
            [6] =
                "r6",
            [7] =
                "r7",
            [8] =
                "r8",
            [9] =
                "r9",
            [10] =
                "r10",
            [11] =
                "r11",
            [12] =
                "r12",
            [13] =
                "sp",
            [14] =
                "lr",
            [15] =
                "pc",
            [25] =
                "cpsr",
        },
        26,
    },
    {
        "arm-vfp.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2008 Free Software Foundation, Inc.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.arm.vfp\">\n"
        "  <reg name=\"d0\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d1\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d2\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d3\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d4\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d5\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d6\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d7\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d8\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d9\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d10\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d11\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d12\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d13\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d14\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d15\" bitsize=\"64\" type=\"float\"/>\n"
        "\n"
        "  <reg name=\"fpscr\" bitsize=\"32\" type=\"int\" group=\"float\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.arm.vfp",
        (const char * const []) {
            [0] =
                "d0",
            [1] =
                "d1",
            [2] =
                "d2",
            [3] =
                "d3",
            [4] =
                "d4",
            [5] =
                "d5",
            [6] =
                "d6",
            [7] =
                "d7",
            [8] =
                "d8",
            [9] =
                "d9",
            [10] =
                "d10",
            [11] =
                "d11",
            [12] =
                "d12",
            [13] =
                "d13",
            [14] =
                "d14",
            [15] =
                "d15",
            [16] =
                "fpscr",
        },
        17,
    },
    {
        "arm-vfp3.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2008 Free Software Foundation, Inc.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.arm.vfp\">\n"
        "  <reg name=\"d0\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d1\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d2\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d3\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d4\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d5\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d6\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d7\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d8\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d9\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d10\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d11\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d12\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d13\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d14\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d15\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d16\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d17\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d18\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d19\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d20\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d21\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d22\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d23\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d24\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d25\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d26\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d27\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d28\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d29\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d30\" bitsize=\"64\" type=\"float\"/>\n"
        "  <reg name=\"d31\" bitsize=\"64\" type=\"float\"/>\n"
        "\n"
        "  <reg name=\"fpscr\" bitsize=\"32\" type=\"int\" group=\"float\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.arm.vfp",
        (const char * const []) {
            [0] =
                "d0",
            [1] =
                "d1",
            [2] =
                "d2",
            [3] =
                "d3",
            [4] =
                "d4",
            [5] =
                "d5",
            [6] =
                "d6",
            [7] =
                "d7",
            [8] =
                "d8",
            [9] =
                "d9",
            [10] =
                "d10",
            [11] =
                "d11",
            [12] =
                "d12",
            [13] =
                "d13",
            [14] =
                "d14",
            [15] =
                "d15",
            [16] =
                "d16",
            [17] =
                "d17",
            [18] =
                "d18",
            [19] =
                "d19",
            [20] =
                "d20",
            [21] =
                "d21",
            [22] =
                "d22",
            [23] =
                "d23",
            [24] =
                "d24",
            [25] =
                "d25",
            [26] =
                "d26",
            [27] =
                "d27",
            [28] =
                "d28",
            [29] =
                "d29",
            [30] =
                "d30",
            [31] =
                "d31",
            [32] =
                "fpscr",
        },
        33,
    },
    {
        "arm-vfp-sysregs.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2021 Linaro Ltd.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.\n"
        "\n"
        "     These are A/R profile VFP system registers. Debugger users probably\n"
        "     don't really care about these, but because we used to (incorrectly)\n"
        "     provide them to gdb in the org.gnu.gdb.arm.vfp XML we continue\n"
        "     to do so via this separate XML.\n"
        "     -->\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.qemu.gdb.arm.vfp-sysregs\">\n"
        "  <reg name=\"fpsid\" bitsize=\"32\" type=\"int\" group=\"float\"/>\n"
        "  <reg name=\"fpexc\" bitsize=\"32\" type=\"int\" group=\"float\"/>\n"
        "</feature>\n",
        "org.qemu.gdb.arm.vfp-sysregs",
        (const char * const []) {
            [0] =
                "fpsid",
            [1] =
                "fpexc",
        },
        2,
    },
    {
        "arm-neon.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2008 Free Software Foundation, Inc.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.arm.vfp\">\n"
        "  <vector id=\"neon_uint8x8\" type=\"uint8\" count=\"8\"/>\n"
        "  <vector id=\"neon_uint16x4\" type=\"uint16\" count=\"4\"/>\n"
        "  <vector id=\"neon_uint32x2\" type=\"uint32\" count=\"2\"/>\n"
        "  <vector id=\"neon_float32x2\" type=\"ieee_single\" count=\"2\"/>\n"
        "  <union id=\"neon_d\">\n"
        "    <field name=\"u8\" type=\"neon_uint8x8\"/>\n"
        "    <field name=\"u16\" type=\"neon_uint16x4\"/>\n"
        "    <field name=\"u32\" type=\"neon_uint32x2\"/>\n"
        "    <field name=\"u64\" type=\"uint64\"/>\n"
        "    <field name=\"f32\" type=\"neon_float32x2\"/>\n"
        "    <field name=\"f64\" type=\"ieee_double\"/>\n"
        "  </union>\n"
        "  <vector id=\"neon_uint8x16\" type=\"uint8\" count=\"16\"/>\n"
        "  <vector id=\"neon_uint16x8\" type=\"uint16\" count=\"8\"/>\n"
        "  <vector id=\"neon_uint32x4\" type=\"uint32\" count=\"4\"/>\n"
        "  <vector id=\"neon_uint64x2\" type=\"uint64\" count=\"2\"/>\n"
        "  <vector id=\"neon_float32x4\" type=\"ieee_single\" count=\"4\"/>\n"
        "  <vector id=\"neon_float64x2\" type=\"ieee_double\" count=\"2\"/>\n"
        "  <union id=\"neon_q\">\n"
        "    <field name=\"u8\" type=\"neon_uint8x16\"/>\n"
        "    <field name=\"u16\" type=\"neon_uint16x8\"/>\n"
        "    <field name=\"u32\" type=\"neon_uint32x4\"/>\n"
        "    <field name=\"u64\" type=\"neon_uint64x2\"/>\n"
        "    <field name=\"f32\" type=\"neon_float32x4\"/>\n"
        "    <field name=\"f64\" type=\"neon_float64x2\"/>\n"
        "  </union>\n"
        "  <reg name=\"d0\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d1\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d2\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d3\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d4\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d5\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d6\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d7\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d8\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d9\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d10\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d11\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d12\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d13\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d14\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d15\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d16\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d17\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d18\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d19\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d20\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d21\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d22\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d23\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d24\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d25\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d26\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d27\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d28\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d29\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d30\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "  <reg name=\"d31\" bitsize=\"64\" type=\"neon_d\"/>\n"
        "\n"
        "  <reg name=\"q0\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q1\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q2\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q3\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q4\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q5\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q6\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q7\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q8\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q9\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q10\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q11\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q12\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q13\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q14\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "  <reg name=\"q15\" bitsize=\"128\" type=\"neon_q\"/>\n"
        "\n"
        "  <reg name=\"fpscr\" bitsize=\"32\" type=\"int\" group=\"float\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.arm.vfp",
        (const char * const []) {
            [0] =
                "d0",
            [1] =
                "d1",
            [2] =
                "d2",
            [3] =
                "d3",
            [4] =
                "d4",
            [5] =
                "d5",
            [6] =
                "d6",
            [7] =
                "d7",
            [8] =
                "d8",
            [9] =
                "d9",
            [10] =
                "d10",
            [11] =
                "d11",
            [12] =
                "d12",
            [13] =
                "d13",
            [14] =
                "d14",
            [15] =
                "d15",
            [16] =
                "d16",
            [17] =
                "d17",
            [18] =
                "d18",
            [19] =
                "d19",
            [20] =
                "d20",
            [21] =
                "d21",
            [22] =
                "d22",
            [23] =
                "d23",
            [24] =
                "d24",
            [25] =
                "d25",
            [26] =
                "d26",
            [27] =
                "d27",
            [28] =
                "d28",
            [29] =
                "d29",
            [30] =
                "d30",
            [31] =
                "d31",
            [32] =
                "q0",
            [33] =
                "q1",
            [34] =
                "q2",
            [35] =
                "q3",
            [36] =
                "q4",
            [37] =
                "q5",
            [38] =
                "q6",
            [39] =
                "q7",
            [40] =
                "q8",
            [41] =
                "q9",
            [42] =
                "q10",
            [43] =
                "q11",
            [44] =
                "q12",
            [45] =
                "q13",
            [46] =
                "q14",
            [47] =
                "q15",
            [48] =
                "fpscr",
        },
        49,
    },
    {
        "arm-m-profile.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2010-2020 Free Software Foundation, Inc.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.arm.m-profile\">\n"
        "  <reg name=\"r0\" bitsize=\"32\"/>\n"
        "  <reg name=\"r1\" bitsize=\"32\"/>\n"
        "  <reg name=\"r2\" bitsize=\"32\"/>\n"
        "  <reg name=\"r3\" bitsize=\"32\"/>\n"
        "  <reg name=\"r4\" bitsize=\"32\"/>\n"
        "  <reg name=\"r5\" bitsize=\"32\"/>\n"
        "  <reg name=\"r6\" bitsize=\"32\"/>\n"
        "  <reg name=\"r7\" bitsize=\"32\"/>\n"
        "  <reg name=\"r8\" bitsize=\"32\"/>\n"
        "  <reg name=\"r9\" bitsize=\"32\"/>\n"
        "  <reg name=\"r10\" bitsize=\"32\"/>\n"
        "  <reg name=\"r11\" bitsize=\"32\"/>\n"
        "  <reg name=\"r12\" bitsize=\"32\"/>\n"
        "  <reg name=\"sp\" bitsize=\"32\" type=\"data_ptr\"/>\n"
        "  <reg name=\"lr\" bitsize=\"32\"/>\n"
        "  <reg name=\"pc\" bitsize=\"32\" type=\"code_ptr\"/>\n"
        "  <reg name=\"xpsr\" bitsize=\"32\" regnum=\"25\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.arm.m-profile",
        (const char * const []) {
            [0] =
                "r0",
            [1] =
                "r1",
            [2] =
                "r2",
            [3] =
                "r3",
            [4] =
                "r4",
            [5] =
                "r5",
            [6] =
                "r6",
            [7] =
                "r7",
            [8] =
                "r8",
            [9] =
                "r9",
            [10] =
                "r10",
            [11] =
                "r11",
            [12] =
                "r12",
            [13] =
                "sp",
            [14] =
                "lr",
            [15] =
                "pc",
            [25] =
                "xpsr",
        },
        26,
    },
    {
        "arm-m-profile-mve.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2021 Free Software Foundation, Inc.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.arm.m-profile-mve\">\n"
        "  <flags id=\"vpr_reg\" size=\"4\">\n"
        "    <!-- ARMv8.1-M and MVE: Unprivileged and privileged Access.  -->\n"
        "    <field name=\"P0\" start=\"0\" end=\"15\"/>\n"
        "    <!-- ARMv8.1-M: Privileged Access only.  -->\n"
        "    <field name=\"MASK01\" start=\"16\" end=\"19\"/>\n"
        "    <!-- ARMv8.1-M: Privileged Access only.  -->\n"
        "    <field name=\"MASK23\" start=\"20\" end=\"23\"/>\n"
        "  </flags>\n"
        "  <reg name=\"vpr\" bitsize=\"32\" type=\"vpr_reg\"/>\n"
        "</feature>\n",
        "org.gnu.gdb.arm.m-profile-mve",
        (const char * const []) {
            [0] =
                "vpr",
        },
        1,
    },
    {
        "aarch64-pauth.xml",
        "<?xml version=\"1.0\"?>\n"
        "<!-- Copyright (C) 2018-2022 Free Software Foundation, Inc.\n"
        "\n"
        "     Copying and distribution of this file, with or without modification,\n"
        "     are permitted in any medium without royalty provided the copyright\n"
        "     notice and this notice are preserved.  -->\n"
        "\n"
        "<!DOCTYPE feature SYSTEM \"gdb-target.dtd\">\n"
        "<feature name=\"org.gnu.gdb.aarch64.pauth_v2\">\n"
        "  <reg name=\"pauth_dmask\" bitsize=\"64\"/>\n"
        "  <reg name=\"pauth_cmask\" bitsize=\"64\"/>\n"
        "  <reg name=\"pauth_dmask_high\" bitsize=\"64\"/>\n"
        "  <reg name=\"pauth_cmask_high\" bitsize=\"64\"/>\n"
        "</feature>\n"
        "\n",
        "org.gnu.gdb.aarch64.pauth_v2",
        (const char * const []) {
            [0] =
                "pauth_dmask",
            [1] =
                "pauth_cmask",
            [2] =
                "pauth_dmask_high",
            [3] =
                "pauth_cmask_high",
        },
        4,
    },
    { NULL }
};
