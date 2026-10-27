#!/usr/bin/env bash
# Build the vendor Vulkan drivers for Android x86_64 with the NDK, out of tree,
# and install them into out/vendor-overlay (mirrors /vendor).
#
#   gpu/build.sh            # everything
#   gpu/build.sh radv       # libdrm + Mesa RADV only
#   gpu/build.sh test       # NDK test programs (gpu/test)
#
# Runs itself inside the gpu build container (gpu/tools.sh); no sudo needed.
set -euo pipefail

# ---- pins -------------------------------------------------------------------
MESA_TAG=mesa-26.2.5
MESA_URL=https://gitlab.freedesktop.org/mesa/mesa.git
LIBDRM_TAG=libdrm-2.4.134
LIBDRM_URL=https://gitlab.freedesktop.org/mesa/libdrm.git
NDK_VERSION=30.0.16248370            # NDK r30
API=37                               # Android 17
# -----------------------------------------------------------------------------

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && git rev-parse --path-format=absolute --git-common-dir | xargs dirname)
if [[ ! -e /run/.containerenv ]]; then
  exec "$here/tools.sh" "$here/build.sh" "$@"
fi

NDK=${NDK:-$HOME/Android/Sdk/ndk/$NDK_VERSION}
[[ -d $NDK ]] || { echo "NDK $NDK_VERSION not found at $NDK" >&2; exit 1; }
ndk_bin=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
ndk_sysroot=$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot

out=${OUT:-$repo/out}
work=$out/gpu
src=$work/src
build=$work/build
stage=$work/stage                     # static libs + pkg-config for Mesa
overlay=$out/vendor-overlay
jobs=$(nproc)
mkdir -p "$src" "$build" "$stage" "$overlay"

# Generated cross file with the absolute NDK path; combined with cross/android-x86_64.ini.
cat > "$build/ndk.ini" <<EOF
[constants]
ndk_bin = '$ndk_bin'
api = '$API'
EOF
cross=(--cross-file "$build/ndk.ini" --cross-file "$here/cross/android-x86_64.ini")

export PKG_CONFIG_LIBDIR=$stage/vendor/lib64/pkgconfig
export PKG_CONFIG_SYSROOT_DIR=$stage
unset PKG_CONFIG_PATH

# fetch <dir> <url> <tag>: shallow clone at a pinned tag, then apply gpu/patches/<dir>/*.patch.
fetch() {
  local dir=$1 url=$2 tag=$3
  if [[ ! -d $src/$dir/.git ]]; then
    git clone -q --depth 1 -b "$tag" "$url" "$src/$dir"
  fi
  local have
  have=$(git -C "$src/$dir" describe --tags --exact-match HEAD 2>/dev/null || true)
  if [[ $have != "$tag" ]]; then
    git -C "$src/$dir" fetch -q --depth 1 origin tag "$tag"
    git -C "$src/$dir" checkout -q -f "$tag"
  fi
  # Re-apply patches from a clean tree so edits to patches take effect.
  git -C "$src/$dir" checkout -q -f "$tag"
  git -C "$src/$dir" clean -qfdx
  if compgen -G "$here/patches/$dir/*.patch" >/dev/null; then
    for p in "$here"/patches/"$dir"/*.patch; do
      git -C "$src/$dir" apply "$p"
    done
  fi
}

# Re-run meson setup on existing build dirs so option edits here take effect.
reconf() { [[ -f $1/build.ninja ]] && echo --reconfigure || true; }

build_libdrm() {
  fetch libdrm "$LIBDRM_URL" "$LIBDRM_TAG"
  local b=$build/libdrm
  meson setup $(reconf "$b") "$b" "$src/libdrm" "${cross[@]}" \
    --prefix=/vendor --libdir=lib64 --datadir=etc \
    -Ddefault_library=static -Dbuildtype=release \
    -Damdgpu=enabled -Dintel=disabled -Dradeon=disabled -Dnouveau=disabled \
    -Dvmwgfx=disabled -Dfreedreno=disabled -Dvc4=disabled -Detnaviv=disabled \
    -Dexynos=disabled -Domap=disabled -Dtegra=disabled -Dcairo-tests=disabled \
    -Dvalgrind=disabled -Dman-pages=disabled -Dtests=false -Dinstall-test-programs=false
  meson compile -C "$b"
  DESTDIR=$stage meson install -C "$b" --quiet
  # libdrm installs amdgpu.ids to <datadir>/libdrm; that path is baked into libdrm_amdgpu.
  install -Dm644 "$stage/vendor/etc/libdrm/amdgpu.ids" "$overlay/etc/libdrm/amdgpu.ids"
}

# zlib for Mesa's shader cache: link the NDK's static libz so the driver does not
# need libz.so (not LL-NDK).
stage_zlib() {
  mkdir -p "$PKG_CONFIG_LIBDIR"
  install -m644 "$ndk_sysroot/usr/lib/x86_64-linux-android/libz.a" "$stage/vendor/lib64/libz.a"
  cat > "$PKG_CONFIG_LIBDIR/zlib.pc" <<'EOF'
Name: zlib
Description: NDK static zlib
Version: 1.3.1
Libs: /vendor/lib64/libz.a
Cflags:
EOF
}

build_radv() {
  build_libdrm
  stage_zlib
  fetch mesa "$MESA_URL" "$MESA_TAG"
  local b=$build/mesa
  meson setup $(reconf "$b") "$b" "$src/mesa" "${cross[@]}" \
    --prefix=/vendor --libdir=lib64 \
    -Dbuildtype=release -Db_ndebug=true \
    -Dplatforms=android -Dplatform-sdk-version=$API \
    -Dandroid-stub=true -Dandroid-libbacktrace=disabled -Dandroid-strict=false \
    -Dvulkan-drivers=amd -Dgallium-drivers= -Dllvm=disabled -Damd-use-llvm=false \
    -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dopengl=false \
    -Dgles1=disabled -Dgles2=disabled \
    -Dzstd=disabled -Dexpat=disabled -Dxmlconfig=disabled \
    -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled \
    -Dperfetto=false -Dvulkan-layers= -Dtools= -Dbuild-tests=false \
    -Dvideo-codecs=all_free -Dspirv-tools=disabled
  meson compile -C "$b"
  local so=$b/src/amd/vulkan/libvulkan_radeon.so
  install -d "$overlay/lib64/hw"
  # libvulkan dlopens /vendor/lib64/hw/vulkan.<ro.hardware.vulkan>.so; give it a matching soname.
  "$ndk_bin/llvm-objcopy" --strip-unneeded "$so" "$build/vulkan.radv.so"
  patchelf --set-soname vulkan.radv.so --remove-rpath "$build/vulkan.radv.so"
  install -m644 "$build/vulkan.radv.so" "$overlay/lib64/hw/vulkan.radv.so"
  check_elf "$overlay/lib64/hw/vulkan.radv.so"
}

# Allowed DT_NEEDED for vendor libraries: LL-NDK + what we ship in the overlay.
LLNDK="libc.so libm.so libdl.so liblog.so libnativewindow.so libsync.so libvndksupport.so \
libbinder_ndk.so libEGL.so libGLESv1_CM.so libGLESv2.so libGLESv3.so libvulkan.so libselinux.so \
libcamera_metadata.so libmediandk.so libapexsupport.so libvendorsupport.so"
check_elf() {
  local f=$1 bad=0
  local needed; needed=$(readelf -dW "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
  for n in $needed; do
    if [[ " $LLNDK " != *" $n "* && ! -e $overlay/lib64/$n ]]; then
      echo "check_elf: $f: disallowed DT_NEEDED $n" >&2; bad=1
    fi
  done
  echo "check_elf: $(basename "$f") NEEDED: $(echo $needed)"
  return $bad
}

build_test() {
  local t=$build/test cc=$ndk_bin/x86_64-linux-android$API-clang
  mkdir -p "$t"
  glslangValidator -V --quiet --vn fill_comp -o "$t/fill_comp.h" "$here/test/fill.comp"
  "$cc" -O2 -Wall -I"$t" -o "$t/vkdriver_test" "$here/test/vkdriver_test.c" -ldl
  # Test-only vendor layer (run-bionic.sh -v): the mock IMapper v5.
  mkdir -p "$build/test-vendor/lib64/hw"
  "$cc" -O2 -Wall -shared -fPIC -fvisibility=hidden -o "$build/test-vendor/lib64/hw/mapper.lapktest.so" "$here/test/mock_mapper.c"
  ls -l "$t"
}

case ${1:-all} in
  libdrm) build_libdrm ;;
  radv) build_radv ;;
  test) build_test ;;
  all) build_radv; build_test ;;
  *) echo "unknown target $1" >&2; exit 1 ;;
esac
