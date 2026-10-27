#!/usr/bin/env bash
# Cross-build upstream iptables with the nf_tables backend (plus libmnl and
# libnftnl) for Android x86_64 with the NDK, out of tree, and stage it into
# out/session/iptables/system/bin (layered over /system by the session).
#
# Why: the image's iptables is the legacy x_tables flavour, whose kernel
# modules a rootless session can't load. The session owns its network
# namespace, where nf_tables works.
#
# Runs itself inside the build container (tools.sh); no sudo needed.
set -euo pipefail

# ---- pins -------------------------------------------------------------------
LIBMNL=libmnl-1.0.5
LIBNFTNL=libnftnl-1.2.8
IPTABLES=iptables-1.8.11
URL=https://www.netfilter.org/projects
NDK_VERSION=30.0.16248370
API=30
# -----------------------------------------------------------------------------

here=$(cd "$(dirname "$0")" && pwd)
if [[ ! -e /run/.containerenv ]]; then
  repo=$(cd "$here/../.." && git rev-parse --path-format=absolute --git-common-dir | xargs dirname)
  exec "$here/tools.sh" env LINUXAPK_OUT="${LINUXAPK_OUT:-$repo/out}" "$here/build.sh" "$@"
fi

NDK=${NDK:-$HOME/Android/Sdk/ndk/$NDK_VERSION}
bin=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
out=$LINUXAPK_OUT
work=$out/session/iptables-build
stage=$work/stage
dest=$out/session/iptables
mkdir -p "$work/src" "$stage"

export CC="$bin/x86_64-linux-android$API-clang"
export AR="$bin/llvm-ar" RANLIB="$bin/llvm-ranlib" STRIP="$bin/llvm-strip"
export PKG_CONFIG_LIBDIR=$stage/lib/pkgconfig PKG_CONFIG_PATH=
host=x86_64-linux-android
jobs=$(nproc)

fetch() {
  local name=$1 proj=$2 ext=xz
  [[ $name == libmnl-* ]] && ext=bz2 # libmnl only ships .tar.bz2
  [[ -d $work/src/$name ]] && return
  curl -fsSL "$URL/$proj/files/$name.tar.$ext" -o "$work/src/$name.tar.$ext"
  tar -xf "$work/src/$name.tar.$ext" -C "$work/src"
}

build_lib() {
  local name=$1 proj=$2
  fetch "$name" "$proj"
  [[ -f $stage/lib/$proj.a ]] && return
  (cd "$work/src/$name" &&
    ./configure --host=$host --prefix="$stage" --enable-static --disable-shared \
      CFLAGS="-O2 -fPIC" >/dev/null &&
    make -j"$jobs" >/dev/null && make install >/dev/null)
}

build_lib $LIBMNL libmnl
build_lib $LIBNFTNL libnftnl

fetch $IPTABLES iptables
# Bionic quirks: <fcntl.h> defines O_PATH, which collides with an enum in
# libxt_cgroup.c; inttypes.h (PRIx64) isn't pulled in
# transitively as with glibc; bionic has no index().
sed -i 's/\bO_PATH\b/O_CGROUP_PATH/g' "$work/src/$IPTABLES/extensions/libxt_cgroup.c"
sed -i 's/\bindex(/strchr(/g' "$work/src/$IPTABLES/extensions/libebt_among.c" # no index() in bionic
(cd "$work/src/$IPTABLES" &&
  ./configure --host=$host --prefix=/system --sbindir=/system/bin \
    --disable-shared --enable-static --enable-nftables --disable-legacy \
    --disable-libipq --disable-nfsynproxy --disable-bpf-compiler \
    --disable-connlabel --without-kernel \
    CFLAGS="-O2 -include inttypes.h" LDFLAGS="-static" >/dev/null &&
  make -j"$jobs" >/dev/null)

rm -rf "$dest"
mkdir -p "$dest/system/bin"
cp "$work/src/$IPTABLES/iptables/xtables-nft-multi" "$dest/system/bin/"
"$STRIP" "$dest/system/bin/xtables-nft-multi"
for n in iptables iptables-restore iptables-save ip6tables ip6tables-restore ip6tables-save; do
  ln -sf xtables-nft-multi "$dest/system/bin/$n"
done
file "$dest/system/bin/xtables-nft-multi"
