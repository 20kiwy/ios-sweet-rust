#!/bin/sh
# install-qemu-prefix.sh — user-space QEMU for this sandbox (no root).
#
# The sandbox resets wipe /home/z; this script rebuilds the QEMU prefix by
# downloading Debian .deb packages with apt-get download and unpacking them
# with dpkg -x into ~/qemu-prefix. Package list = the dependency closure of
# qemu-system-arm on Debian trixie, found iteratively (see worklog tasks 12-14).
#
# Usage:  sh tools/install-qemu-prefix.sh
# Result: ~/qemu-prefix/bin/qemu  (wrapper script; set QEMU_BIN to override)
set -e

PREFIX="$HOME/qemu-prefix"
ARCH="$(dpkg --print-architecture 2>/dev/null || uname -m)"
case "$ARCH" in
  x86_64|amd64) DARCH=amd64; LIB=usr/lib/x86_64-linux-gnu ;;
  aarch64|arm64) DARCH=arm64; LIB=usr/lib/aarch64-linux-gnu ;;
  *) echo "unknown arch $ARCH"; exit 1 ;;
esac

PKGS="qemu-system-arm qemu-system-data libglib2.0-0t64 libpixman-1-0 \
libcapstone5 librdmacm1t64 libvdeplug2t64 libfuse3-4 libndctl6 libnl-3-200 \
libkmod2 libpmem1 libibverbs1 libaio1t64 libnl-route-3-200 libdaxctl1 \
libslirp0 liburing2 libfdt1 libepoxy0 libdrm2 libgbm1 libseccomp2 \
libguestfs0 libdaemon0"

mkdir -p "$PREFIX/debs" && cd "$PREFIX/debs"
for p in $PKGS; do
  apt-get download "$p" 2>/dev/null || echo "[skip] $p (not available)"
done
for d in ./*.deb; do
  [ -e "$d" ] || continue
  dpkg -x "$d" "$PREFIX/"
done

QEMU="$PREFIX/usr/bin/qemu-system-aarch64"
[ -x "$QEMU" ] || { echo "qemu-system-aarch64 not extracted"; exit 1; }

mkdir -p "$PREFIX/bin"
cat > "$PREFIX/bin/qemu" <<EOF
#!/bin/sh
export LD_LIBRARY_PATH="$PREFIX/$LIB:\$LD_LIBRARY_PATH"
exec "$QEMU" -L "$PREFIX/usr/share/qemu" "\$@"
EOF
chmod +x "$PREFIX/bin/qemu"
"$PREFIX/bin/qemu" --version
echo "[ok] QEMU prefix ready: $PREFIX/bin/qemu"
