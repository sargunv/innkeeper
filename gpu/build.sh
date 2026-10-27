#!/usr/bin/env bash
# Build the vendor Vulkan drivers for Android x86_64 with the NDK, out of tree,
# and install them into out/vendor-overlay (mirrors /vendor).
#
#   gpu/build.sh            # everything
#   gpu/build.sh radv       # libdrm + Mesa RADV only
#   gpu/build.sh lvp        # LLVM + Mesa lavapipe (software fallback)
#   gpu/build.sh test       # NDK test programs (gpu/test), then run them
#
# Builds inside the gpu build container (gpu/tools.sh); no sudo needed. The
# tests run on the host through tests/bionic/run.sh, on lavapipe and, when
# there is an amdgpu render node, on RADV. Env: LINUXAPK_OUT (default
# <main checkout>/out).
set -euo pipefail

# ---- pins -------------------------------------------------------------------
MESA_TAG=mesa-26.2.5
MESA_URL=https://gitlab.freedesktop.org/mesa/mesa.git
LIBDRM_TAG=libdrm-2.4.134
LIBDRM_URL=https://gitlab.freedesktop.org/mesa/libdrm.git
LLVM_TAG=llvmorg-19.1.7 # for lavapipe; same as Mesa CI's Android LLVM
LLVM_URL=https://github.com/llvm/llvm-project.git
NDK_VERSION=30.0.16248370 # NDK r30
API=37                    # Android 17
# -----------------------------------------------------------------------------

here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
main=$(cd "$repo" && git rev-parse --path-format=absolute --git-common-dir | xargs dirname)
out=${LINUXAPK_OUT:-$main/out}
build=$out/gpu/build

# Run the test programs (build_test) on each driver.
run_tests() {
  local drivers=(lvp) n d
  for n in /dev/dri/renderD*; do
    if [[ $(readlink -f "/sys/class/drm/${n##*/}/device/driver") == */amdgpu ]]; then
      drivers+=(radv)
      break
    fi
  done
  for d in "${drivers[@]}"; do
    echo "== gpu tests: $d"
    driver_tests "$d"
  done
}

driver_tests() {
  local d=$1 t=/data/nativetest64
  bionic() {
    "$repo/tests/bionic/run.sh" -t "$build/test" -v "$build/test-vendor" \
      -e "LINUXAPK_PROPS=ro.hardware.vulkan=$d;ro.hardware.egl=angle" "$@"
  }
  local vk=("$t/vkdriver_test" --hal "vulkan.$d.so")
  bionic -- "${vk[@]}" --udmabuf
  # --anb hands the driver a deliberately wrong stride: it passes only if the
  # layout came from the mapper (mock_mapper.c here, ours with --anb-hal).
  bionic -e MESA_GRALLOC_MAPPER_SUFFIX=lapktest -- "${vk[@]}" --anb
  bionic -e MESA_GRALLOC_MAPPER_SUFFIX=lapktest -- "${vk[@]}" --anb --deferred
  bionic -- "${vk[@]}" --anb-hal --deferred
  bionic -- "$t/vkdriver_test" --loader
  # fake_sf stands in for SurfaceFlinger's getDisplayStats.
  local sf="$t/fake_sf & until service check SurfaceFlingerAIDL | grep -q ': found'; do sleep 0.05; done"
  bionic -- /system/bin/sh -c "$sf; $t/swapchain_test"
  bionic -- /system/bin/sh -c "$sf; $t/swapchain_test --deferred"
  bionic -- "$t/ahb_vk_test"
  bionic -- "$t/gles_test"
  bionic -- "$t/gles_yuv_test"
}

if [[ ! -e /run/.containerenv ]]; then
  "$here/tools.sh" "$here/build.sh" "$@"
  case ${1:-all} in test | all) run_tests ;; esac
  exit
fi

NDK=${NDK:-$HOME/Android/Sdk/ndk/$NDK_VERSION}
[[ -d $NDK ]] || {
  echo "NDK $NDK_VERSION not found at $NDK" >&2
  exit 1
}
ndk_bin=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
ndk_sysroot=$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot

work=$out/gpu
src=$work/src
stage=$work/stage # static libs + pkg-config for Mesa
overlay=$out/vendor-overlay
jobs=$(nproc)
mkdir -p "$src" "$build" "$stage" "$overlay"

# Generated cross file with the absolute NDK path; combined with cross/android-x86_64.ini.
cat >"$build/ndk.ini" <<EOF
[constants]
ndk_bin = '$ndk_bin'
api = '$API'
EOF
cross=(--cross-file "$build/ndk.ini" --cross-file "$here/cross/android-x86_64.ini")

export PKG_CONFIG_LIBDIR=$stage/vendor/lib64/pkgconfig
export PKG_CONFIG_SYSROOT_DIR=$stage
unset PKG_CONFIG_PATH

# fetch <dir> <url> <tag>: shallow clone at a pinned tag, then apply
# gpu/patches/<dir>/*.patch. Resets the tree only when the tag or patches change.
fetch() {
  local dir=$1 url=$2 tag=$3
  if [[ ! -d $src/$dir/.git ]]; then
    git clone -q --depth 1 -b "$tag" "$url" "$src/$dir"
  fi
  local stamp=$work/stamps/$dir want
  want=$({
    echo "$tag"
    cat "$here"/patches/"$dir"/*.patch 2>/dev/null || true
  } | sha256sum)
  [[ -f $stamp && $(<"$stamp") == "$want" ]] && return 0
  if ! git -C "$src/$dir" rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    git -C "$src/$dir" fetch -q --depth 1 origin tag "$tag"
  fi
  git -C "$src/$dir" checkout -q -f "$tag"
  git -C "$src/$dir" clean -qfdx
  if compgen -G "$here/patches/$dir/*.patch" >/dev/null; then
    for p in "$here"/patches/"$dir"/*.patch; do
      git -C "$src/$dir" apply "$p"
    done
  fi
  mkdir -p "$work/stamps"
  echo "$want" >"$stamp"
}

# Re-run meson setup on existing build dirs so option edits here take effect.
reconf() { [[ -f $1/build.ninja ]] && echo --reconfigure || true; }

build_libdrm() {
  fetch libdrm "$LIBDRM_URL" "$LIBDRM_TAG"
  local b=$build/libdrm
  # shellcheck disable=SC2046 # reconf prints zero or one flag
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
  cat >"$PKG_CONFIG_LIBDIR/zlib.pc" <<'EOF'
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
  # shellcheck disable=SC2046 # reconf prints zero or one flag
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
  install_driver "$b/src/amd/vulkan/libvulkan_radeon.so" radv
}

# LLVM static libraries for lavapipe, as in Mesa's
# .gitlab-ci/container/build-android-x86_64-llvm.sh.
build_llvm() {
  fetch llvm-project "$LLVM_URL" "$LLVM_TAG"
  local b=$build/llvm prefix=$stage/llvm
  [[ -f $prefix/include/llvm/Config/llvm-config.h ]] && return 0
  cmake -GNinja -S "$src/llvm-project/llvm" -B "$b" \
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI=x86_64 -DANDROID_PLATFORM=android-$API -DANDROID_NDK="$NDK" \
    -DCMAKE_BUILD_TYPE=MinSizeRel -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_CXX_FLAGS="-march=x86-64 -fno-rtti" \
    -DLLVM_HOST_TRIPLE=x86_64-linux-android$API -DLLVM_TARGETS_TO_BUILD=X86 \
    -DLLVM_BUILD_LLVM_DYLIB=OFF -DLLVM_BUILD_TESTS=OFF -DLLVM_BUILD_EXAMPLES=OFF \
    -DLLVM_BUILD_DOCS=OFF -DLLVM_BUILD_TOOLS=OFF -DLLVM_INCLUDE_TESTS=OFF \
    -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_ENABLE_RTTI=OFF \
    -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_LIBXML2=OFF \
    -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_OPTIMIZED_TABLEGEN=ON
  ninja -C "$b" -j"$jobs" install
}

# Meson subproject wrapping the static LLVM (Mesa's create-llvm-meson-wrap-file.sh).
write_llvm_wrap() {
  local prefix=$stage/llvm d=$src/mesa/subprojects/llvm libs ver
  mkdir -p "$d"
  libs=$(cd "$prefix/lib" && printf '%s\n' libLLVM*.a | sed "s/\.a$//; s/.*/'&'/" | paste -sd,)
  ver=$(sed -n 's/^#define LLVM_VERSION_STRING "\([^"]*\)".*/\1/p' "$prefix/include/llvm/Config/llvm-config.h")
  cat >"$d/meson.build" <<WRAP
project('llvm', ['cpp'])
cpp = meson.get_compiler('cpp')
_deps = []
foreach d : [$libs]
  _deps += cpp.find_library(d, dirs : '$prefix/lib', static : true)
endforeach
dep_llvm = declare_dependency(
  include_directories : include_directories('$prefix/include'),
  dependencies : _deps,
  version : '$ver',
)
has_rtti = false
irbuilder_h = files('$prefix/include/llvm/IR/IRBuilder.h')
WRAP
}

build_lvp() {
  build_llvm
  stage_zlib
  fetch mesa "$MESA_URL" "$MESA_TAG"
  write_llvm_wrap
  local b=$build/mesa-lvp
  # shellcheck disable=SC2046 # reconf prints zero or one flag
  meson setup $(reconf "$b") "$b" "$src/mesa" "${cross[@]}" \
    --prefix=/vendor --libdir=lib64 --force-fallback-for=llvm \
    -Dbuildtype=release -Db_ndebug=true -Dcpp_rtti=false \
    -Dplatforms=android -Dplatform-sdk-version=$API \
    -Dandroid-stub=true -Dandroid-libbacktrace=disabled -Dandroid-strict=false \
    -Dvulkan-drivers=swrast -Dgallium-drivers=llvmpipe -Dllvm=enabled -Dshared-llvm=disabled \
    -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dopengl=false \
    -Dgles1=disabled -Dgles2=disabled \
    -Dzstd=disabled -Dexpat=disabled -Dxmlconfig=disabled \
    -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled \
    -Dperfetto=false -Dvulkan-layers= -Dtools= -Dbuild-tests=false \
    -Dvideo-codecs= -Dspirv-tools=disabled -Ddraw-use-llvm=true
  meson compile -C "$b"
  install_driver "$b/src/gallium/targets/lavapipe/libvulkan_lvp.so" lvp
}

# install_driver <built .so> <name>: install as /vendor/lib64/hw/vulkan.<name>.so.
install_driver() {
  local so=$1 name=$2 dst=$build/vulkan.$2.so
  install -d "$overlay/lib64/hw"
  # libvulkan dlopens vulkan.<ro.hardware.vulkan>.so; give it a matching soname.
  "$ndk_bin/llvm-objcopy" --strip-unneeded "$so" "$dst"
  patchelf --set-soname "vulkan.$name.so" --remove-rpath "$dst"
  install -m644 "$dst" "$overlay/lib64/hw/vulkan.$name.so"
  check_elf "$overlay/lib64/hw/vulkan.$name.so"
}

# Allowed DT_NEEDED for vendor libraries: LL-NDK + what we ship in the overlay.
LLNDK="libc.so libm.so libdl.so liblog.so libnativewindow.so libsync.so libvndksupport.so \
libbinder_ndk.so libEGL.so libGLESv1_CM.so libGLESv2.so libGLESv3.so libvulkan.so libselinux.so \
libcamera_metadata.so libmediandk.so libapexsupport.so libvendorsupport.so"
check_elf() {
  local f=$1 bad=0
  local needed
  needed=$(readelf -dW "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
  for n in $needed; do
    if [[ " $LLNDK " != *" $n "* && ! -e $overlay/lib64/$n ]]; then
      echo "check_elf: $f: disallowed DT_NEEDED $n" >&2
      bad=1
    fi
  done
  # A HAL module should export HMI and nothing else (no static LLVM/libc++).
  local exports
  exports=$(readelf -sW --dyn-syms "$f" | awk '$7 != "UND" && ($5 == "GLOBAL" || $5 == "WEAK") {print $8}' | sort -u)
  if [[ $exports != HMI ]]; then
    echo "check_elf: $f exports $(echo "$exports" | wc -l) symbols, expected only HMI" >&2
    bad=1
  fi
  echo "check_elf: $(basename "$f") NEEDED: ${needed//$'\n'/ }; exports: $(echo "$exports" | cut -c1-80)"
  return $bad
}

# Property defaults and Vulkan feature XMLs (gpu/overlay -> out/vendor-overlay).
install_etc() {
  cp -r "$here/overlay/." "$overlay/"
}

build_test() {
  local t=$build/test cc=$ndk_bin/x86_64-linux-android$API-clang
  mkdir -p "$t"
  glslangValidator -V --quiet --vn fill_comp -o "$t/fill_comp.h" "$here/test/fill.comp"
  "$cc" -O2 -Wall -I"$t" -o "$t/vkdriver_test" "$here/test/vkdriver_test.c" -ldl
  "$cc" -O2 -Wall -o "$t/gles_test" "$here/test/gles_test.c" -lEGL -lGLESv3
  "$cc" -O2 -Wall -o "$t/gles_yuv_test" "$here/test/gles_yuv_test.c" -lEGL -lGLESv3 -lnativewindow
  "$cc" -O2 -Wall -o "$t/ahb_vk_test" "$here/test/ahb_vk_test.c" -lvulkan -lnativewindow
  "$cc" -O2 -Wall -o "$t/swapchain_test" "$here/test/swapchain_test.c" -lvulkan -lmediandk -lnativewindow
  "$cc" -O2 -Wall -o "$t/fake_sf" "$here/test/fake_sf.c" -lbinder_ndk -ldl
  # vkdriver_test --anb-hal: buffers from our allocator's code.
  local hal=$repo/hal
  "$cc++" -O2 -std=c++20 -shared -fPIC -fvisibility=hidden -static-libstdc++ \
    -I"$hal/common/include" -o "$t/libhalbuf.so" "$here/test/halbuf.cpp" \
    "$hal"/common/src/{buffer_alloc,layout,native_handle}.cpp -llog
  # Test-only vendor layer (tests/bionic/run.sh -v): the mock IMapper v5.
  mkdir -p "$build/test-vendor/lib64/hw"
  "$cc" -O2 -Wall -shared -fPIC -fvisibility=hidden -o "$build/test-vendor/lib64/hw/mapper.lapktest.so" "$here/test/mock_mapper.c"
  ls -l "$t"
}

case ${1:-all} in
  libdrm) build_libdrm ;;
  radv)
    build_radv
    install_etc
    ;;
  llvm) build_llvm ;;
  lvp) build_lvp ;;
  test) build_test ;;
  all)
    build_radv
    build_lvp
    install_etc
    build_test
    ;;
  *)
    echo "unknown target $1" >&2
    exit 1
    ;;
esac
