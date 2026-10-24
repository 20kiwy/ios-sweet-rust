#!/bin/sh
# build.sh -- build qemu-sweet: QEMU v10.0.0 + Apple T8015 display-path
# device models (sweet-dart / sweet-disp0 / sweet-mipi-dsim) for the
# ios-sweet-rust project.
#
# THE SOURCE IS VENDORED: qemu-sweet/qemu/ IS the pristine QEMU v10.0.0
# tree (byte-identical to the upstream tag, verified by diff at vendor
# time).  No network clone is needed -- clone this repo, run this script.
#
# Idempotent: re-runnable.  Restores every source path we touch (git
# reset when the tree is a git repo; reverse-apply of our wiring patch
# otherwise -- the vendored tree has no .git), re-copies the overlay,
# re-applies 0001-sweet-wire-device-models.patch, configures a FRESH
# build dir, builds qemu-system-aarch64 and installs it into $QEMU_PREFIX.
#
# Env:
#   QEMU_SRC      QEMU v10.0.0 source      (default <this dir>/qemu, the
#                 vendored tree; an external clone also works)
#   QEMU_PREFIX   install prefix           (default /home/z/qemu-prefix-sweet)
#   FAST=1        resume mode: reuse build-sweet/ if it is already
#                 configured (long first builds survive a killed shell;
#                 ninja picks up incrementally).  Default = full clean.
#
# Only these paths inside $QEMU_SRC are ever modified:
#   hw/misc/, hw/display/, include/hw/arm/, include/monitor/hmp.h,
#   hmp-commands.hx, subprojects/<pre-cloned wraps> (sandbox gitlab is
#   blocked), build-sweet/ (build dir, wiped each run unless FAST=1).
set -e

# The sandbox ships meson/ninja in /home/z/.venv/bin; on CI the distro
# toolchain is usually first on PATH already.  Prepending is harmless.
export PATH="/home/z/.venv/bin:$PATH"

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
QEMU_SRC="${QEMU_SRC:-$HERE/qemu}"
QEMU_PREFIX="${QEMU_PREFIX:-/home/z/qemu-prefix-sweet}"
PATCH="$HERE/0001-sweet-wire-device-models.patch"

CC=gcc
export CC

echo "build.sh: QEMU_SRC=$QEMU_SRC"
echo "build.sh: QEMU_PREFIX=$QEMU_PREFIX"

if [ ! -f "$QEMU_SRC/VERSION" ]; then
    echo "build.sh: FATAL: $QEMU_SRC does not look like a QEMU source tree" >&2
    exit 1
fi
ver=$(cat "$QEMU_SRC/VERSION" 2>/dev/null || true)
echo "build.sh: source version: $ver"

# Is $QEMU_SRC its OWN git repo (an external clone)?  The vendored tree
# qemu-sweet/qemu/ has no .git of its own -- it merely SITS INSIDE this
# project's repo, and every `git -C $QEMU_SRC ...` command would climb to
# the outer repo and operate on the WRONG worktree (measured: an apply
# that reports success while touching nothing -> a stock-QEMU build with
# zero diagnostics).  So: git commands only for a STANDALONE clone;
# plain patch(1) -- zero repo awareness -- for the vendored tree.
QEMU_SRC_ABS=$(CDPATH= cd -- "$QEMU_SRC" && pwd)
is_standalone_git() {
    top=$(git -C "$QEMU_SRC_ABS" rev-parse --show-toplevel 2>/dev/null) || return 1
    [ "$top" = "$QEMU_SRC_ABS" ]
}

# --------------------------------------------------------------- reset ----
# Return every path we touch to pristine, THEN delete our added files BY
# NAME.  Two mechanisms, both safe to run repeatedly and in any order:
#   * standalone git clone: git checkout (exact);
#   * vendored tree: REVERSE-APPLY of the wiring patch via patch(1)
#     (--forward: when the patch is not currently applied this skips
#     cleanly instead of hanging on the "assume -R?" question).
if is_standalone_git; then
    git -C "$QEMU_SRC_ABS" reset -q
    git -C "$QEMU_SRC_ABS" checkout -- hw include hmp-commands.hx 2>/dev/null || true
else
    ( cd "$QEMU_SRC_ABS" && patch -p1 -R --forward --silent < "$PATCH" ) 2>/dev/null || true
    find "$QEMU_SRC_ABS/hw" "$QEMU_SRC_ABS/include" -name '*.rej' -delete 2>/dev/null || true
    rm -f "$QEMU_SRC_ABS/hmp-commands.hx.rej"
fi
rm -f "$QEMU_SRC_ABS/hw/misc/sweet_dart.c" \
      "$QEMU_SRC_ABS/hw/misc/sweet_mipi.c" \
      "$QEMU_SRC_ABS/hw/display/sweet_disp0.c" \
      "$QEMU_SRC_ABS/include/hw/arm/sweet_devices.h"

# -------------------------------------------------------------- overlay ---
cp "$HERE/overlay/include/hw/arm/sweet_devices.h" "$QEMU_SRC_ABS/include/hw/arm/sweet_devices.h"
cp "$HERE/overlay/hw/misc/sweet_dart.c"           "$QEMU_SRC_ABS/hw/misc/sweet_dart.c"
cp "$HERE/overlay/hw/misc/sweet_mipi.c"           "$QEMU_SRC_ABS/hw/misc/sweet_mipi.c"
cp "$HERE/overlay/hw/display/sweet_disp0.c"       "$QEMU_SRC_ABS/hw/display/sweet_disp0.c"

# ---------------------------------------------------------------- patch ---
if is_standalone_git; then
    git -C "$QEMU_SRC_ABS" apply "$PATCH"
else
    ( cd "$QEMU_SRC_ABS" && patch -p1 --forward --silent < "$PATCH" )
fi
# HARD GATE: a silent no-op apply produced a stock QEMU once -- never again.
grep -q "sweet_dart" "$QEMU_SRC_ABS/hw/misc/meson.build" \
  && grep -qi "config sweet" "$QEMU_SRC_ABS/hw/misc/Kconfig" \
  && grep -q "sweet" "$QEMU_SRC_ABS/hw/arm/virt.c" \
  || { echo "build.sh: FATAL: wiring patch did not land" \
            "(sweet_dart missing from meson.build/Kconfig/virt.c)" >&2; exit 1; }
echo "build.sh: wiring patch applied"

# ------------------------------------------ sandbox subproject fallback ---
# aarch64 'virt' requires libfdt and the ui code requires keycodemapdb.
# Their .wrap files point at gitlab.com, which is NOT reachable from this
# sandbox, so pre-populate those meson subprojects from the qemu-project
# GitHub mirrors (same content) at the exact wrap revision.  CI boxes with
# working gitlab access skip nothing here -- the clones are simply reused.
for name in dtc keycodemapdb berkeley-softfloat-3 berkeley-testfloat-3; do
    wrap="$QEMU_SRC/subprojects/$name.wrap"
    dir="$QEMU_SRC/subprojects/$name"
    rev=$(sed -n 's/^revision *= *//p' "$wrap" 2>/dev/null | head -n 1)
    [ -n "$rev" ] || continue
    if [ -d "$dir" ] && git -C "$dir" rev-parse -q --verify HEAD >/dev/null 2>&1; then
        continue # already populated
    fi
    rm -rf "$dir"
    echo "build.sh: pre-cloning subproject $name (wrap rev $rev) from GitHub mirror"
    git clone --depth 1 "https://github.com/qemu/$name.git" "$dir" 2>/dev/null || {
        echo "build.sh: WARNING: cannot clone $name mirror; meson will try the wrap URL" >&2
        continue
    }
    git -C "$dir" fetch --depth 1 origin "$rev" 2>/dev/null || true
    git -C "$dir" checkout --detach -q "$rev" 2>/dev/null ||
        echo "build.sh: WARNING: $name left at mirror HEAD; meson will reconcile" >&2
    # Replicate meson's wrap patch_directory step (meson only applies it
    # when IT creates the checkout; e.g. the berkeley repos need their
    # qemu meson.build from subprojects/packagefiles/).
    if [ -d "$QEMU_SRC/subprojects/packagefiles/$name" ]; then
        cp -r "$QEMU_SRC/subprojects/packagefiles/$name/." "$dir/"
    fi
done

# ------------------------------------------------------------- configure --
# QEMU v10.0.0 has NO -Dtarget_list meson option: the target list travels
# via TARGET_DIRS in config-host.mak, which only ./configure generates, so
# configure is the entry point.  Out-of-tree build only (QEMU >= 8 removed
# in-tree builds).  Optional deps use 'auto' and degrade gracefully; the
# explicit --disable-* list keeps the dependency footprint minimal.
#
# FAST=1 keeps an already-configured build-sweet/ (resume after an
# interrupted long build; ninja continues incrementally).  The overlay +
# wiring above are source-only, so meson re-runs on the next ninja call.
need_configure=1
if [ "${FAST:-0}" = "1" ] && [ -f "$QEMU_SRC/build-sweet/config-host.mak" ]; then
    need_configure=0
    echo "build.sh: FAST=1 -- reusing configured build-sweet/"
fi
if [ "$need_configure" = "1" ]; then
    rm -rf "$QEMU_SRC/build-sweet"
    mkdir -p "$QEMU_SRC/build-sweet"
    (
        cd "$QEMU_SRC/build-sweet"
        if "$QEMU_SRC/configure" \
            --target-list=aarch64-softmmu \
            --cc="$CC" \
            --disable-docs \
            --disable-tools \
            --disable-guest-agent \
            --disable-strip \
            --disable-debug-info \
            --disable-slirp \
            --disable-seccomp \
            --disable-libnfs \
            --disable-usb-redir \
            --disable-spice \
            --disable-opengl \
            --disable-gtk \
            --disable-sdl \
            --disable-vnc \
            --disable-xkbcommon \
            --disable-auth-pam \
            --disable-glusterfs \
            --disable-libssh \
            --disable-lzo \
            --disable-snappy \
            --disable-curl \
            --disable-gnutls \
            --disable-nettle \
            --disable-gcrypt \
            --disable-brlapi \
            --disable-rbd \
            --disable-libiscsi \
            > configure-sweet.log 2>&1; then
            echo "build.sh: configure OK"
            tail -n 3 configure-sweet.log
        else
            status=$?
            echo "build.sh: configure FAILED (exit $status); last 40 lines:" >&2
            tail -n 40 configure-sweet.log >&2
            exit "$status"
        fi
    )
fi

# ----------------------------------------------------------------- build --
ninja -C "$QEMU_SRC/build-sweet" -j "$(nproc)" qemu-system-aarch64

# --------------------------------------------------------------- install --
mkdir -p "$QEMU_PREFIX/bin"
cp "$QEMU_SRC/build-sweet/qemu-system-aarch64" "$QEMU_PREFIX/bin/qemu-system-aarch64"
{
    git -C "$QEMU_SRC" describe --tags 2>/dev/null || echo "v10.0.0-sweet"
} > "$QEMU_PREFIX/qemu-sweet-version.txt"

echo "build.sh: installed $QEMU_PREFIX/bin/qemu-system-aarch64 (version: $(cat "$QEMU_PREFIX/qemu-sweet-version.txt"))"
echo "build.sh: done.  Boot with:"
echo "  $QEMU_PREFIX/bin/qemu-system-aarch64 -M virt -device sweet-dart -device sweet-mipi-dsim -device sweet-disp0 ..."
