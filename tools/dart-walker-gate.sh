#!/bin/sh
# dart-walker-gate.sh -- run the W3 gate (tools/dart-walker-gate.py)
# against a LIVE boot-qemu session of this repo's payload.
#
# Boots <PAYLOAD> on qemu-sweet with --sweet-devices + --qtest, pauses
# the guest vCPU, drives the stage-2 DART/disp0/MIPI registers exactly
# the way the real drivers would (qtest), and verifies:
#   * unconfigured DART DENIES DMA and latches FAULT_*,
#   * a guest-programmed PTE walks a NON-identity IOVA to the right
#     host page (pattern read-back through the DART AS),
#   * the translation cache serves stale PTEs until STREAM_COMMAND
#     invalidate,
#   * the MIPI command FIFO acks packets.
#
# Usage:  sh tools/dart-walker-gate.sh <PAYLOAD> <OUTDIR> [SECS]
# Result: <OUTDIR>/dart-gate-verdict.json (exit 0 iff pass)
set -e
PAYLOAD="$1"
OUTDIR="$2"
SECS="${3:-60}"
REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

[ -x "$REPO/qemu-prefix-sweet/bin/qemu-system-aarch64" ] || {
    echo "dart-gate: qemu-sweet not built (sh qemu-sweet/build.sh first)" >&2
    exit 1
}
[ -f "$PAYLOAD" ] || { echo "dart-gate: no payload $PAYLOAD" >&2; exit 1; }

mkdir -p "$OUTDIR"

export PATH="$REPO/qemu-prefix-sweet/bin:$PATH"
"$REPO/target/release/sweet-rust" boot-qemu "$PAYLOAD" "$OUTDIR" \
    --secs "$SECS" --sweet-devices --qtest "$OUTDIR/qtest.sock" &
BOOT_PID=$!

python3 "$REPO/tools/dart-walker-gate.py" \
    "$OUTDIR/qtest.sock" "$OUTDIR/mon.sock" "$OUTDIR"
GATE_RC=$?

# Let the boot settle a moment so boot-qemu's own bookkeeping finishes
# cleanly, then report.
wait $BOOT_PID || true
exit $GATE_RC
