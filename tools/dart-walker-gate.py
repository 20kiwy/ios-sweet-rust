#!/usr/bin/env python3
"""dart-walker-gate.py -- the W3 gate (docs/PLAN-FULL-BOOT.md), as a test.

"A guest-programmed PTE translates a DMA to the right host page
(read-back of a known pattern through the DART AS); an IOMFB-initiated
FB flip changes fb without host help."

This drives the stage-2 sweet-dart / sweet-disp0 models through their
MMIO registers from the host, using QEMU's qtest protocol (the same
interface QEMU's own device tests use).  No kernel-side driver is
needed -- there is none (W2 verdict: the booted SWEET kernel never
programs the DART), so the gate synthesizes the exact register traffic
AppleT8015DART / the IOMFB user client would produce:

  1. DENY:   TCR TRANSLATE_ENABLE with TTBR0[0][0]=0 -> disp0 swap DMA
             is denied, DART latches FAULT_STATUS/FAULT_IOVA, FB
             unchanged.
  2. WALK:   tables in guest RAM (L1/L2/L3, 4KB granule, stt-idx-width
             10 -- the real D22AP ADT parameters), TTBR0[0][0]
             programmed through the DART MMIO -> a guest-triggered swap
             (disp0 SWAP_CTRL register write, the IOMFB flip) DMAs the
             surface through the DART at a NON-identity IOVA and the
             framebuffer shows the pattern.
  3. TLB:    rewriting a leaf PTE in RAM alone does NOT change the next
             flip (translation cache); STREAM_COMMAND invalidate does.

Usage:
  python3 tools/dart-walker-gate.py <qtest-sock> <qmp-sock> <outdir>

Writes <outdir>/dart-gate-verdict.json and exits 0 only if every gate
passes.  Run against a live boot-qemu session started with --qtest
(and --sweet-devices); the guest vCPU is paused (QMP stop) for the
duration so the kernel cannot race the framebuffer checks.
"""

import json
import os
import socket
import struct
import sys
import time

# --- the model's register contract (qemu-sweet/overlay, stage 2) -------
DART_BASE = 0x18704000
TCR0 = DART_BASE + 0x000
STREAM_COMMAND = DART_BASE + 0x020
STREAM_SELECT = DART_BASE + 0x034
FAULT_STATUS = DART_BASE + 0x040
FAULT_IOVA_LO = DART_BASE + 0x044
FAULT_IOVA_HI = DART_BASE + 0x048
TTBR00 = DART_BASE + 0x300  # TTBR0[sid 0][slot 0] (stage-2 layout)

DISP0_BASE = 0x18000000
INTSTATUS = DISP0_BASE + 0x48
SWAP_SRC_LO = DISP0_BASE + 0x200
SWAP_SRC_HI = DISP0_BASE + 0x204
SWAP_CTRL = DISP0_BASE + 0x208
SWAP_COUNT = DISP0_BASE + 0x20c

MIPI_BASE = 0x18400000
MIPI_VERSION = MIPI_BASE + 0x00
MIPI_GEN_HDR = MIPI_BASE + 0x48
MIPI_GEN_PLD = MIPI_BASE + 0x4C
MIPI_CMD_PKT_STATUS = MIPI_BASE + 0x50
MIPI_PHY_STATUS = MIPI_BASE + 0x58
MIPI_INTSTATUS = MIPI_BASE + 0x60

FB_BASE = 0x9C000000
FB_W, FB_H = 1080, 2400
FB_SIZE = FB_W * FB_H * 4  # 10,368,000 bytes BGRA

# --- guest RAM layout for the walk tables + pattern pages --------------
# Top of the 2 GiB guest (0x40000000..0xC0000000), far from the payload,
# the ADT and the framebuffer.  5 L3 tables cover the whole surface
# window (2 MiB per L3 table, 10.4 MB surface).
L1_TABLE = 0xBF000000
L2_TABLE = 0xBF001000
L3_BASE = 0xBF002000  # L3 tables i at L3_BASE + 0x1000*i, i = 0..4
PATTERN1_PAGE = 0xBF007000  # words 0xA1B2C3D4
PATTERN2_PAGE = 0xBF008000  # words 0x5E6F7081
SURFACE_IOVA = 0x70000000  # DART IOVA: NOT identity to the pattern pages
P1 = 0xA1B2C3D4
P2 = 0x5E6F7081

PTE_VALID = 0x1

# Walk geometry (page 4K, L3 9 bits, L2 9 bits, L1/stt 10 bits):
# one L3 table = 512 * 4 KiB = 2 MiB of IOVA.
PAGE_SHIFT = 12
L1_IDX = (SURFACE_IOVA >> (PAGE_SHIFT + 9 + 9)) & 0x3FF  # = 1
L2_IDX = (SURFACE_IOVA >> (PAGE_SHIFT + 9)) & 0x1FF  # = 896&511 = 384
N_L3 = (FB_SIZE + 0x1FFFFF) // 0x200000  # 5 L3 tables = 10 MiB window


class Qtest:
    """Minimal qtest text-protocol client (unix socket)."""

    def __init__(self, path, timeout=30.0):
        deadline = time.time() + timeout
        last = None
        while True:
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.s.connect(path)
                break
            except OSError as e:
                last = e
                if time.time() > deadline:
                    raise SystemExit(
                        f"dart-gate: cannot connect qtest socket {path}: {last}")
                time.sleep(0.25)
        self.s.settimeout(10.0)
        self.buf = b""

    def _cmd(self, line):
        self.s.sendall(line.encode() + b"\n")
        while True:
            nl = self.buf.find(b"\n")
            if nl >= 0:
                resp = self.buf[:nl].decode()
                self.buf = self.buf[nl + 1:]
                return resp
            d = self.s.recv(65536)
            if not d:
                raise SystemExit("dart-gate: qtest connection closed")
            self.buf += d

    def writel(self, addr, val):
        r = self._cmd(f"writel 0x{addr:x} 0x{val & 0xffffffff:x}")
        if not r.startswith("OK"):
            raise SystemExit(f"dart-gate: writel failed: {r}")

    def readl(self, addr):
        r = self._cmd(f"readl 0x{addr:x}")
        if not r.startswith("OK"):
            raise SystemExit(f"dart-gate: readl failed: {r}")
        return int(r.split()[1], 16)

    def write_mem(self, addr, data):
        # qtest v10 'write ADDR LEN DATA': DATA is ONE contiguous hex
        # string ("0x" + 2 hex chars per byte), parsed from words[3].
        off = 0
        while off < len(data):
            chunk = data[off:off + 256]
            r = self._cmd(f"write 0x{addr + off:x} 0x{len(chunk):x} "
                          f"0x{chunk.hex()}")
            if not r.startswith("OK"):
                raise SystemExit(f"dart-gate: write failed: {r}")
            off += len(chunk)

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


class Qmp:
    """Minimal one-shot QMP client (stop/cont/pmemsave)."""

    def __init__(self, path, timeout=30.0):
        deadline = time.time() + timeout
        last = None
        while True:
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.s.connect(path)
                break
            except OSError as e:
                last = e
                if time.time() > deadline:
                    raise SystemExit(
                        f"dart-gate: cannot connect QMP socket {path}: {last}")
                time.sleep(0.25)
        self.s.settimeout(30.0)
        self.buf = b""
        self._read_line()  # greeting ({"QMP": ...} -- no "return" key)
        self.cmd("qmp_capabilities")

    def _read_line(self):
        while True:
            nl = self.buf.find(b"\n")
            if nl >= 0:
                line = self.buf[:nl]
                self.buf = self.buf[nl + 1:]
                return line
            d = self.s.recv(65536)
            if not d:
                raise SystemExit("dart-gate: QMP connection closed")
            self.buf += d

    def _read_until_return(self):
        while True:
            line = self._read_line()
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "return" in msg:
                return msg["return"]
            if "error" in msg:
                raise SystemExit(f"dart-gate: QMP error: {msg}")

    def cmd(self, name, args=None):
        m = {"execute": name}
        if args:
            m["arguments"] = args
        self.s.sendall(json.dumps(m).encode() + b"\n")
        return self._read_until_return()

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


def pmemsave(qmp, addr, size, path):
    qmp.cmd("pmemsave", {"val": addr, "size": size, "filename": path})
    # QEMU writes asynchronously; poll for the file to reach full size.
    deadline = time.time() + 60
    while time.time() < deadline:
        if os.path.exists(path) and os.path.getsize(path) >= size:
            return
        time.sleep(0.1)
    raise SystemExit(f"dart-gate: pmemsave {path} never reached {size} bytes")


def fb_words_match(data, word, lo_word=None, lo_word_idx=0):
    """Every 4-byte word == `word`, except index lo_word_idx (page 0 of the
    surface, only used by the partial-reprogram check) which must equal
    lo_word when given."""
    step = 4
    for i in range(0, len(data), step):
        w = struct.unpack_from("<I", data, i)[0]
        if lo_word is not None and i == lo_word_idx * 4:
            if w != lo_word:
                return False
        elif w != word:
            return False
    return True


def main():
    if len(sys.argv) != 4:
        print(__doc__)
        raise SystemExit(2)
    qt_path, qmp_path, outdir = sys.argv[1], sys.argv[2], sys.argv[3]
    os.makedirs(outdir, exist_ok=True)

    verdict = {"gates": {}, "detail": {}}

    qt = Qtest(qt_path)
    qmp = Qmp(qmp_path)

    # Freeze the guest: no kernel framebuffer traffic can race the checks.
    qmp.cmd("stop")
    print("dart-gate: vCPU stopped, driving the device models via qtest")

    try:
        # -------------------------------------------------- baseline ---
        intstatus0 = qt.readl(INTSTATUS)
        count0 = qt.readl(SWAP_COUNT)
        tcr0 = qt.readl(TCR0)
        verdict["detail"]["baseline"] = {
            "intstatus": intstatus0, "swap_count": count0, "tcr0": tcr0,
        }
        verdict["gates"]["baseline_swap_count_zero"] = count0 == 0

        # -------------------------------------------------- deny -------
        # Translate on, no table: every DMA must be denied and latched.
        qt.writel(TCR0, 0x1)  # TRANSLATE_ENABLE, no BYPASS
        qt.writel(SWAP_SRC_LO, SURFACE_IOVA)
        qt.writel(SWAP_SRC_HI, 0)
        qt.writel(SWAP_CTRL, 0x1)  # SWAP_GO
        time.sleep(0.2)
        fs = qt.readl(FAULT_STATUS)
        flo = qt.readl(FAULT_IOVA_LO)
        fhi = qt.readl(FAULT_IOVA_HI)
        cnt1 = qt.readl(SWAP_COUNT)
        intst1 = qt.readl(INTSTATUS)
        verdict["detail"]["deny"] = {
            "fault_status": fs, "fault_iova": fhi << 32 | flo,
            "swap_count": cnt1, "intstatus": intst1,
        }
        verdict["gates"]["deny_fault_latched"] = bool(fs & 1) \
            and SURFACE_IOVA <= (fhi << 32 | flo) \
            < SURFACE_IOVA + FB_SIZE
        verdict["gates"]["deny_fb_unchanged"] = cnt1 == 0 \
            and not (intst1 & 1)

        # Ack the fault (W1C): status clears, IRQ would deassert.
        qt.writel(FAULT_STATUS, 0x1)
        verdict["gates"]["deny_fault_ack_clears"] = qt.readl(FAULT_STATUS) == 0

        # -------------------------------------------------- program ----
        # Tables in guest RAM (qtest writes), DART via its own MMIO.
        pattern1 = struct.pack("<I", P1) * (0x1000 // 4)
        pattern2 = struct.pack("<I", P2) * (0x1000 // 4)
        qt.write_mem(PATTERN1_PAGE, pattern1)
        qt.write_mem(PATTERN2_PAGE, pattern2)

        qt.writel(TTBR00, L1_TABLE | PTE_VALID)  # auto-flushes the TLB
        l1 = [0] * 1024
        l1[L1_IDX] = L2_TABLE | PTE_VALID
        qt.write_mem(L1_TABLE, struct.pack("<1024Q", *l1))
        l2 = [0] * 512
        for i in range(N_L3):
            l2[L2_IDX + i] = (L3_BASE + 0x1000 * i) | PTE_VALID
        qt.write_mem(L2_TABLE, struct.pack("<512Q", *l2))
        l3 = [PATTERN1_PAGE | PTE_VALID] * 512  # every surface page -> P1
        for i in range(N_L3):
            qt.write_mem(L3_BASE + 0x1000 * i, struct.pack("<512Q", *l3))
        qt.writel(STREAM_SELECT, 0x1)  # (scope evidence; model flushes all)
        qt.writel(STREAM_COMMAND, 0x1)  # explicit INVALIDATE_ALL

        # -------------------------------------------------- walk -------
        # The IOMFB flip: surface IOVA -> DART walk -> framebuffer.
        qt.writel(SWAP_CTRL, 0x1)
        time.sleep(0.2)
        cnt2 = qt.readl(SWAP_COUNT)
        intst2 = qt.readl(INTSTATUS)
        verdict["detail"]["walk"] = {"swap_count": cnt2, "intstatus": intst2}
        verdict["gates"]["walk_swap_completed"] = cnt2 == 1 \
            and bool(intst2 & 1)
        qt.writel(INTSTATUS, 0x1)  # W1C SWAP_DONE

        fb_raw = os.path.join(outdir, "fb-gate-pattern1.raw")
        pmemsave(qmp, FB_BASE, FB_SIZE, fb_raw)
        fb = open(fb_raw, "rb").read()
        ok1 = fb_words_match(fb, P1)
        verdict["gates"]["walk_pte_to_right_page"] = ok1
        verdict["detail"]["walk"]["fb_matches_pattern1"] = ok1
        if not ok1:
            for i in range(0, min(len(fb), 4096), 4):
                w = struct.unpack_from("<I", fb, i)[0]
                if w != P1:
                    verdict["detail"]["walk"]["first_bad_word"] = \
                        {"index": i // 4, "value": w}
                    break

        # -------------------------------------------------- TLB --------
        # Rewrite ONE leaf PTE in RAM: the cached translation must serve
        # the OLD page until STREAM_COMMAND invalidate.
        l3_a0 = [PATTERN2_PAGE | PTE_VALID]
        l3_rest = [PATTERN1_PAGE | PTE_VALID] * 511
        qt.write_mem(L3_BASE, struct.pack("<512Q", *(l3_a0 + l3_rest)))
        qt.writel(SWAP_CTRL, 0x1)  # flip #2: should be STALE (cache)
        time.sleep(0.2)
        fb_raw2 = os.path.join(outdir, "fb-gate-tlb-stale.raw")
        pmemsave(qmp, FB_BASE, FB_SIZE, fb_raw2)
        fb2 = open(fb_raw2, "rb").read()
        stale_ok = fb_words_match(fb2, P1)
        verdict["gates"]["tlb_serves_stale_until_invalidate"] = stale_ok

        qt.writel(STREAM_COMMAND, 0x1)  # INVALIDATE_ALL
        qt.writel(SWAP_CTRL, 0x1)  # flip #3: fresh walk -> P2 on page 0
        time.sleep(0.2)
        fb_raw3 = os.path.join(outdir, "fb-gate-tlb-fresh.raw")
        pmemsave(qmp, FB_BASE, FB_SIZE, fb_raw3)
        fb3 = open(fb_raw3, "rb").read()
        fresh_ok = (
            # page 0 (words 0..1023) must be P2 now; page 1+ stays P1
            struct.unpack_from("<I", fb3, 0)[0] == P2
            and struct.unpack_from("<I", fb3, 4096)[0] == P1)
        verdict["gates"]["tlb_invalidate_refetches"] = fresh_ok

        # --------------------------------------- MIPI command FIFO -----
        ver = qt.readl(MIPI_VERSION)
        qt.writel(MIPI_GEN_PLD, 0xDEADBEEF)
        qt.writel(MIPI_GEN_HDR, 0x13A5501)  # a DCS long-write header
        st = qt.readl(MIPI_CMD_PKT_STATUS)
        ist = qt.readl(MIPI_INTSTATUS)
        hdr_rb = qt.readl(MIPI_GEN_HDR)
        verdict["detail"]["mipi"] = {
            "version": ver, "cmd_pkt_status": st, "intstatus": ist,
            "hdr_readback": hdr_rb,
        }
        verdict["gates"]["mipi_version_curated"] = ver == 0x3133302A
        verdict["gates"]["mipi_cmd_fifo_acks"] = \
            st == 0x5 and bool(ist & 1) and hdr_rb == 0x13A5501
    finally:
        qmp.cmd("cont")
        print("dart-gate: vCPU resumed")
        qt.close()
        qmp.close()

    verdict["pass"] = all(verdict["gates"].values())
    path = os.path.join(outdir, "dart-gate-verdict.json")
    with open(path, "w") as f:
        json.dump(verdict, f, indent=2)
        f.write("\n")
    print(json.dumps(verdict, indent=2))
    if not verdict["pass"]:
        failed = [k for k, v in verdict["gates"].items() if not v]
        raise SystemExit(f"dart-gate: FAILED gates: {failed}")
    print("dart-gate: PASS")


if __name__ == "__main__":
    main()
