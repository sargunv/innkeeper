#!/usr/bin/env bash
# Part of build:system. Cross-build upstream iptables with the nf_tables backend
# (plus libmnl and libnftnl) for Android x86_64 with the NDK pin, in the build
# container, and install it into out/build/system/install's /system/bin, with
# the linuxapk-iptables front end in place of the iptables commands netd runs.
#
# Why: the image's iptables is the legacy x_tables flavour, whose kernel
# modules a rootless session can't load. The session owns its network
# namespace, where nf_tables works.
set -euo pipefail

# ---- pins -------------------------------------------------------------------
LIBMNL=libmnl-1.0.5
LIBNFTNL=libnftnl-1.2.8
IPTABLES=iptables-1.8.11
URL=https://www.netfilter.org/projects
# -----------------------------------------------------------------------------

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
if [[ ! -e /run/.containerenv ]]; then
  exec "$repo/tools/container/run.sh" "$here/build.sh"
fi

bin=$ANDROID_HOME/ndk/$ANDROID_NDK_VERSION/toolchains/llvm/prebuilt/linux-x86_64/bin
work=$repo/out/build/iptables
deps=$work/deps
dest=$repo/out/build/system/install/share/linuxapk/android/system/bin
mkdir -p "$work/src" "$deps"

export CC="$bin/x86_64-linux-android$ANDROID_API-clang"
export AR="$bin/llvm-ar" RANLIB="$bin/llvm-ranlib" STRIP="$bin/llvm-strip"
export PKG_CONFIG_LIBDIR=$deps/lib/pkgconfig PKG_CONFIG_PATH=
host=x86_64-linux-android

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
  [[ -f $deps/lib/$proj.a ]] && return
  (cd "$work/src/$name" &&
    ./configure --host=$host --prefix="$deps" --enable-static --disable-shared \
      CFLAGS="-O2 -fPIC" >/dev/null &&
    make -j"$JOBS" >/dev/null && make install >/dev/null)
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
  make -j"$JOBS" >/dev/null)

mkdir -p "$dest"
cp "$work/src/$IPTABLES/iptables/xtables-nft-multi" "$dest/"
"$STRIP" "$dest/xtables-nft-multi"
for n in iptables-save ip6tables-save; do
  ln -sf xtables-nft-multi "$dest/$n"
done
# The front end, and the commands netd runs, linked to it.
install -m 755 "$here/linuxapk-iptables" "$dest/"
for n in iptables iptables-restore ip6tables ip6tables-restore; do
  cp -P "$here/$n" "$dest/"
done
file "$dest/xtables-nft-multi"
