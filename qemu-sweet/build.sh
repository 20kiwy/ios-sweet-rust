#!/bin/sh
# build.sh -- build qemu-sweet: QEMU v10.0.0 + Apple T8015 display-path
# device models (sweet-dart / sweet-disp0 / sweet-mipi-dsim) for the
# ios-sweet-rust project.
#
# Idempotent: re-runnable.  Resets every source path we touch to pristine,
# re-copies the overlay, re-applies the wiring patch, configures a FRESH
# build dir, builds qemu-system-aarch64 and installs it into $QEMU_PREFIX.
#
# Env:
#   QEMU_SRC      pristine QEMU v10.0.0 clone (default /home/z/qemu-src)
#   QEMU_PREFIX   install prefix            (default /home/z/qemu-prefix-sweet)
#
# Only these paths inside $QEMU_SRC are ever modified:
#   hw/misc/, hw/display/, include/hw/arm/, include/monitor/hmp.h,
#   hmp-commands.hx, subprojects/<pre-cloned wraps> (sandbox gitlab is
#   blocked), build-sweet/ (build dir, wiped each run).
set -e

# The sandbox ships meson/ninja in /home/z/.venv/bin; on CI the distro
# toolchain is usually first on PATH already.  Prepending is harmless.
export PATH="/home/z/.venv/bin:$PATH"

QEMU_SRC="${QEMU_SRC:-/home/z/qemu-src}"
QEMU_PREFIX="${QEMU_PREFIX:-/home/z/qemu-prefix-sweet}"
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PATCH="$HERE/0001-sweet-wire-device-models.patch"

CC=gcc
export CC

echo "build.sh: QEMU_SRC=$QEMU_SRC"
echo "build.sh: QEMU_PREFIX=$QEMU_PREFIX"

# --------------------------------------------------------------- reset ----
# Restore every tracked path we touch; delete our added files BY NAME so a
# blanket `git clean` can never remove unrelated content.
git -C "$QEMU_SRC" reset -q 2>/dev/null || true
git -C "$QEMU_SRC" checkout -- hw include hmp-commands.hx 2>/dev/null || true
rm -f "$QEMU_SRC/hw/misc/sweet_dart.c" \
      "$QEMU_SRC/hw/misc/sweet_mipi.c" \
      "$QEMU_SRC/hw/display/sweet_disp0.c" \
      "$QEMU_SRC/include/hw/arm/sweet_devices.h"

# -------------------------------------------------------------- overlay ---
cp "$HERE/overlay/include/hw/arm/sweet_devices.h" "$QEMU_SRC/include/hw/arm/sweet_devices.h"
cp "$HERE/overlay/hw/misc/sweet_dart.c"           "$QEMU_SRC/hw/misc/sweet_dart.c"
cp "$HERE/overlay/hw/misc/sweet_mipi.c"           "$QEMU_SRC/hw/misc/sweet_mipi.c"
cp "$HERE/overlay/hw/display/sweet_disp0.c"       "$QEMU_SRC/hw/display/sweet_disp0.c"

# ---------------------------------------------------------------- patch ---
git -C "$QEMU_SRC" apply "$PATCH"
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
