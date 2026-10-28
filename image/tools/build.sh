#!/usr/bin/env bash
# build:image-tools: the tools that unpack the image at setup, built static from
# pinned upstream sources in the build container (tools/container/run.sh), so
# preparing an image needs nothing from the host or a container. Into
# out/build/image-tools/install/libexec/linuxapk:
#   simg2img, lpunpack  super.img (sparse) to partition images (android-tools)
#   fsck.erofs          extracts the erofs partitions (erofs-utils, static lz4)
#   debugfs             extracts the ext4 APEX payloads (e2fsprogs)
# Skipped when its inputs (this directory, the container) still hash to
# out/build/image-tools/.stamp.
set -euo pipefail

# ---- pins -------------------------------------------------------------------
# name url sha256
sources=(
  "lz4-1.10.0 https://github.com/lz4/lz4/releases/download/v1.10.0/lz4-1.10.0.tar.gz 537512904744b35e232912055ccf8ec66d768639ff3abe5788d90d792ec5f48b"
  "erofs-utils-1.9.4 https://git.kernel.org/pub/scm/linux/kernel/git/xiang/erofs-utils.git/snapshot/erofs-utils-1.9.4.tar.gz 7d135aa2550326a5acf20f53c518aea5a8900015ce50700044e40f818c31dd80"
  "e2fsprogs-1.47.4 https://mirrors.edge.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v1.47.4/e2fsprogs-1.47.4.tar.xz fd5bf388cbdbe006a3d3b318d983b2948382440acc85a87f1e7d108653e8db0b"
  "android-tools-37.0.0p1 https://github.com/nmeum/android-tools/releases/download/37.0.0p1/android-tools-37.0.0p1.tar.xz 2c6de1abe16b211c7d9f59276217e14fd9e0e18b20e331d3c84358f5dffd576e"
)
# -----------------------------------------------------------------------------

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
work=$repo/out/build/image-tools

if [[ ! -e /run/.containerenv ]]; then
  inputs=$(cd "$repo" && find image/tools tools/container/Containerfile -type f -print0 |
    sort -z | xargs -0 sha256sum | sha256sum)
  if [[ -d $work/install && $(cat "$work/.stamp" 2>/dev/null) == "$inputs" ]]; then
    echo "image-tools: up to date"
    exit 0
  fi
  rm -f "$work/.stamp"
  "$repo/tools/container/run.sh" "$here/build.sh"
  echo "$inputs" >"$work/.stamp"
  exit
fi

src=$work/src deps=$work/deps build=$work/build
bin=$work/install/libexec/linuxapk
rm -rf "$work/install" "$build" "$deps"
mkdir -p "$src" "$build" "$deps" "$bin"

# Unpacks each pinned tarball into $src/<name> (once), after checking its hash.
for s in "${sources[@]}"; do
  read -r name url sha <<<"$s"
  [[ -d $src/$name ]] && continue
  tarball=$src/$(basename "$url")
  curl -fsSL --retry 3 -o "$tarball" "$url"
  echo "$sha  $tarball" | sha256sum -c --quiet
  tar -xf "$tarball" -C "$src"
  rm "$tarball"
done

make -C "$src/lz4-1.10.0/lib" -j"$JOBS" -s install PREFIX="$deps" BUILD_SHARED=no

(cd "$src/erofs-utils-1.9.4" && { [[ -x configure ]] || ./autogen.sh >/dev/null; })
mkdir -p "$build/erofs-utils"
(cd "$build/erofs-utils" &&
  PKG_CONFIG_LIBDIR=$deps/lib/pkgconfig PKG_CONFIG_PATH='' \
    "$src/erofs-utils-1.9.4/configure" --enable-lz4 --disable-lzma --without-zlib \
    --without-libzstd --without-xxhash --without-uuid --without-libcurl --without-openssl \
    --without-libxml2 --without-json-c --without-libnl3 >/dev/null &&
  make -j"$JOBS" -s -C lib && make -j"$JOBS" -s -C fsck LDFLAGS=-all-static)
install -m 755 -s "$build/erofs-utils/fsck/fsck.erofs" "$bin/"

mkdir -p "$build/e2fsprogs"
(cd "$build/e2fsprogs" &&
  "$src/e2fsprogs-1.47.4/configure" --enable-libuuid --enable-libblkid --disable-nls \
    --disable-fuse2fs --disable-uuidd --without-libarchive LDFLAGS=-static >/dev/null &&
  make -j"$JOBS" -s libs >/dev/null && make -j"$JOBS" -s -C debugfs debugfs >/dev/null)
install -m 755 -s "$build/e2fsprogs/debugfs/debugfs" "$bin/"

cmake -S "$src/android-tools-37.0.0p1" -B "$build/android-tools" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DANDROID_TOOLS_USE_BUNDLED_FMT=ON \
  -DCMAKE_EXE_LINKER_FLAGS=-static >/dev/null
ninja -C "$build/android-tools" -j "$JOBS" simg2img lpunpack >/dev/null
install -m 755 -s "$build/android-tools/vendor/simg2img" "$build/android-tools/vendor/lpunpack" "$bin/"

file "$bin"/*
