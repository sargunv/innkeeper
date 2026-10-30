#!/usr/bin/env bash
# build:vulkan: the vendor Vulkan drivers (Mesa RADV, ANV and lavapipe) for Android
# x86_64, cross-built with the NDK pin in the build container
# (tools/container/run.sh), into out/build/vulkan/install:
#   share/linuxapk/android/vendor/lib64/hw/vulkan.{radv,anv,lvp}.so
#   share/linuxapk/android/vendor/etc/libdrm/amdgpu.ids
#   share/linuxapk/props.d/vulkan.prop
# and the test programs tests/run.sh runs (out/build/vulkan/{test,test-vendor}).
# Skipped when its inputs (this directory, the container, the toolchain pin)
# still hash to out/build/vulkan/.stamp.
set -euo pipefail

# ---- pins -------------------------------------------------------------------
MESA_TAG=mesa-26.2.5
MESA_URL=https://gitlab.freedesktop.org/mesa/mesa.git
LIBDRM_TAG=libdrm-2.4.134
LIBDRM_URL=https://gitlab.freedesktop.org/mesa/libdrm.git
LLVM_TAG=llvmorg-19.1.7 # for lavapipe; same as Mesa CI's Android LLVM
LLVM_URL=https://github.com/llvm/llvm-project.git
# -----------------------------------------------------------------------------

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
work=$repo/out/build/vulkan

if [[ ! -e /run/.containerenv ]]; then
  # tests/halbuf.cpp builds the graphics HAL's buffer code.
  inputs=$(
    cd "$repo" &&
      find android/vendor/vulkan android/vendor/graphics/common tools/container/Containerfile \
        tools/build/toolchain.env -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum
  )
  if [[ -d $work/install && $(cat "$work/.stamp" 2>/dev/null) == "$inputs" ]]; then
    echo "vulkan: up to date"
    exit 0
  fi
  rm -f "$work/.stamp"
  "$repo/tools/container/run.sh" "$here/build.sh"
  echo "$inputs" >"$work/.stamp"
  exit
fi

API=$ANDROID_API
NDK=$ANDROID_HOME/ndk/$ANDROID_NDK_VERSION
[[ -d $NDK ]] || {
  echo "NDK $ANDROID_NDK_VERSION not found at $NDK" >&2
  exit 1
}
ndk_bin=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
ndk_sysroot=$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot

src=$work/src
build=$work/build
deps=$work/deps # static libs + pkg-config for Mesa
vendor=$work/install/share/linuxapk/android/vendor
rm -rf "$work/install"
mkdir -p "$src" "$build" "$deps" "$vendor"

# Generated cross file with the absolute NDK path; combined with cross/android-x86_64.ini.
cat >"$build/ndk.ini" <<EOF
[constants]
ndk_bin = '$ndk_bin'
api = '$API'
EOF
cross=(--cross-file "$build/ndk.ini" --cross-file "$here/cross/android-x86_64.ini")

export PKG_CONFIG_LIBDIR=$deps/vendor/lib64/pkgconfig
export PKG_CONFIG_SYSROOT_DIR=$deps
unset PKG_CONFIG_PATH

# fetch <dir> <url> <tag>: shallow clone at a pinned tag, then apply
# patches/<dir>/*.patch. Resets the tree only when the tag or patches change.
fetch() {
  local dir=$1 url=$2 tag=$3
  if [[ ! -d $src/$dir/.git ]]; then
    git clone -q --depth 1 -b "$tag" "$url" "$src/$dir"
  fi
  local stamp=$work/fetched/$dir want
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
  mkdir -p "$work/fetched"
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
  meson compile -C "$b" -j "$JOBS"
  DESTDIR=$deps meson install -C "$b" --quiet
  # libdrm installs amdgpu.ids to <datadir>/libdrm; that path is baked into libdrm_amdgpu.
  install -Dm644 "$deps/vendor/etc/libdrm/amdgpu.ids" "$vendor/etc/libdrm/amdgpu.ids"
}

# zlib for Mesa's shader cache: link the NDK's static libz so the driver does not
# need libz.so (not LL-NDK).
static_zlib() {
  mkdir -p "$PKG_CONFIG_LIBDIR"
  install -m644 "$ndk_sysroot/usr/lib/x86_64-linux-android/libz.a" "$deps/vendor/lib64/libz.a"
  cat >"$PKG_CONFIG_LIBDIR/zlib.pc" <<'EOF'
Name: zlib
Description: NDK static zlib
Version: 1.3.1
Libs: /vendor/lib64/libz.a
Cflags:
EOF
}

# ANV compiles internal OpenCL shaders at build time. These native tools stay
# in the container; LLVM/Clang are never linked into the Android hardware HALs.
build_shader_tools() {
  fetch mesa "$MESA_URL" "$MESA_TAG"
  local b=$build/mesa-tools
  # Do not search the Android dependencies when configuring native tools.
  # shellcheck disable=SC2046 # reconf prints zero or one flag
  env -u PKG_CONFIG_LIBDIR -u PKG_CONFIG_SYSROOT_DIR meson setup $(reconf "$b") "$b" "$src/mesa" \
    -Dbuildtype=release -Dplatforms= -Dvulkan-drivers= -Dgallium-drivers= \
    -Dmesa-clc=enabled -Dinstall-mesa-clc=true -Dllvm=enabled -Dshared-llvm=enabled \
    -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dopengl=false \
    -Dgles1=disabled -Dgles2=disabled -Dbuild-tests=false
  meson compile -C "$b" -j "$JOBS" mesa_clc vtn_bindgen2
  export PATH="$b/src/compiler/clc:$b/src/compiler/spirv:$PATH"
}

build_hardware() {
  build_shader_tools
  build_libdrm
  static_zlib
  fetch mesa "$MESA_URL" "$MESA_TAG"
  local b=$build/mesa
  # shellcheck disable=SC2046 # reconf prints zero or one flag
  meson setup $(reconf "$b") "$b" "$src/mesa" "${cross[@]}" \
    --prefix=/vendor --libdir=lib64 \
    -Dbuildtype=release -Db_ndebug=true \
    -Dplatforms=android -Dplatform-sdk-version="$API" \
    -Dandroid-stub=true -Dandroid-libbacktrace=disabled -Dandroid-strict=false \
    -Dvulkan-drivers=amd,intel -Dgallium-drivers= -Dllvm=disabled -Damd-use-llvm=false \
    -Dmesa-clc=system -Dintel-rt=disabled \
    -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dopengl=false \
    -Dgles1=disabled -Dgles2=disabled \
    -Dzstd=disabled -Dexpat=disabled -Dxmlconfig=disabled \
    -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled \
    -Dperfetto=false -Dvulkan-layers= -Dtools= -Dbuild-tests=false \
    -Dvideo-codecs=all_free -Dspirv-tools=disabled
  meson compile -C "$b" -j "$JOBS"
  install_driver "$b/src/amd/vulkan/libvulkan_radeon.so" radv
  install_driver "$b/src/intel/vulkan/libvulkan_intel.so" anv
  python3 "$here/anv-pci-ids.py" "$src/mesa" >"$work/install/share/linuxapk/anv-pci-ids"
}

# LLVM static libraries for lavapipe, as in Mesa's
# .gitlab-ci/container/build-android-x86_64-llvm.sh.
build_llvm() {
  fetch llvm-project "$LLVM_URL" "$LLVM_TAG"
  local b=$build/llvm prefix=$deps/llvm
  [[ -f $prefix/include/llvm/Config/llvm-config.h ]] && return 0
  cmake -GNinja -S "$src/llvm-project/llvm" -B "$b" \
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI=x86_64 -DANDROID_PLATFORM=android-"$API" -DANDROID_NDK="$NDK" \
    -DCMAKE_BUILD_TYPE=MinSizeRel -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_CXX_FLAGS="-march=x86-64 -fno-rtti" \
    -DLLVM_HOST_TRIPLE=x86_64-linux-android"$API" -DLLVM_TARGETS_TO_BUILD=X86 \
    -DLLVM_BUILD_LLVM_DYLIB=OFF -DLLVM_BUILD_TESTS=OFF -DLLVM_BUILD_EXAMPLES=OFF \
    -DLLVM_BUILD_DOCS=OFF -DLLVM_BUILD_TOOLS=OFF -DLLVM_INCLUDE_TESTS=OFF \
    -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_ENABLE_RTTI=OFF \
    -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_LIBXML2=OFF \
    -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_OPTIMIZED_TABLEGEN=ON
  ninja -C "$b" -j "$JOBS" install
}

# Meson subproject wrapping the static LLVM (Mesa's create-llvm-meson-wrap-file.sh).
write_llvm_wrap() {
  local prefix=$deps/llvm d=$src/mesa/subprojects/llvm libs ver
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
  static_zlib
  fetch mesa "$MESA_URL" "$MESA_TAG"
  write_llvm_wrap
  local b=$build/mesa-lvp
  # shellcheck disable=SC2046 # reconf prints zero or one flag
  meson setup $(reconf "$b") "$b" "$src/mesa" "${cross[@]}" \
    --prefix=/vendor --libdir=lib64 --force-fallback-for=llvm \
    -Dbuildtype=release -Db_ndebug=true -Dcpp_rtti=false \
    -Dplatforms=android -Dplatform-sdk-version="$API" \
    -Dandroid-stub=true -Dandroid-libbacktrace=disabled -Dandroid-strict=false \
    -Dvulkan-drivers=swrast -Dgallium-drivers=llvmpipe -Dllvm=enabled -Dshared-llvm=disabled \
    -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dopengl=false \
    -Dgles1=disabled -Dgles2=disabled \
    -Dzstd=disabled -Dexpat=disabled -Dxmlconfig=disabled \
    -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled \
    -Dperfetto=false -Dvulkan-layers= -Dtools= -Dbuild-tests=false \
    -Dvideo-codecs= -Dspirv-tools=disabled -Ddraw-use-llvm=true
  meson compile -C "$b" -j "$JOBS"
  install_driver "$b/src/gallium/targets/lavapipe/libvulkan_lvp.so" lvp
}

# install_driver <built .so> <name>: install as /vendor/lib64/hw/vulkan.<name>.so.
install_driver() {
  local so=$1 name=$2 dst=$build/vulkan.$2.so
  install -d "$vendor/lib64/hw"
  # libvulkan dlopens vulkan.<ro.hardware.vulkan>.so; give it a matching soname.
  "$ndk_bin/llvm-objcopy" --strip-unneeded "$so" "$dst"
  patchelf --set-soname "vulkan.$name.so" --remove-rpath "$dst"
  install -m644 "$dst" "$vendor/lib64/hw/vulkan.$name.so"
  check_elf "$vendor/lib64/hw/vulkan.$name.so"
}

# Allowed DT_NEEDED for vendor libraries: LL-NDK + what we ship in /vendor/lib64.
LLNDK="libc.so libm.so libdl.so liblog.so libnativewindow.so libsync.so libvndksupport.so \
libbinder_ndk.so libEGL.so libGLESv1_CM.so libGLESv2.so libGLESv3.so libvulkan.so libselinux.so \
libcamera_metadata.so libmediandk.so libapexsupport.so libvendorsupport.so"
check_elf() {
  local f=$1 bad=0
  local needed
  needed=$(readelf -dW "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
  for n in $needed; do
    if [[ " $LLNDK " != *" $n "* && ! -e $vendor/lib64/$n ]]; then
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

# Graphics properties for the launcher; feature XMLs are written at start.
install_etc() {
  install -D -m644 -t "$work/install/share/linuxapk/props.d" "$here/etc/vulkan.prop"
}

# Test programs for tests/run.sh: $work/test (bound at /data/nativetest64)
# and a test-only vendor layer, $work/test-vendor.
build_test() {
  local t=$work/test cc=$ndk_bin/x86_64-linux-android$API-clang
  rm -rf "$t" "$work/test-vendor"
  mkdir -p "$t" "$work/test-vendor/lib64/hw"
  glslangValidator -V --quiet --vn fill_comp -o "$build/fill_comp.h" "$here/tests/fill.comp"
  "$cc" -O2 -Wall -I"$build" -o "$t/vkdriver_test" "$here/tests/vkdriver_test.c" -ldl
  "$cc" -O2 -Wall -o "$t/gles_test" "$here/tests/gles_test.c" -lEGL -lGLESv3
  "$cc" -O2 -Wall -o "$t/gles_yuv_test" "$here/tests/gles_yuv_test.c" -lEGL -lGLESv3 -lnativewindow
  "$cc" -O2 -Wall -o "$t/ahb_vk_test" "$here/tests/ahb_vk_test.c" -lvulkan -lnativewindow
  "$cc" -O2 -Wall -o "$t/swapchain_test" "$here/tests/swapchain_test.c" -lvulkan -lmediandk -lnativewindow
  "$cc" -O2 -Wall -o "$t/fake_sf" "$here/tests/fake_sf.c" -lbinder_ndk -ldl
  # vkdriver_test --anb-hal: buffers from our allocator's code.
  local graphics=$repo/android/vendor/graphics
  "$cc++" -O2 -std=c++20 -shared -fPIC -fvisibility=hidden -static-libstdc++ \
    -I"$graphics/common/include" -o "$t/libhalbuf.so" "$here/tests/halbuf.cpp" \
    "$graphics"/common/src/{buffer_alloc,layout,native_handle}.cpp -llog
  # The mock IMapper v5.
  "$cc" -O2 -Wall -shared -fPIC -fvisibility=hidden -o "$work/test-vendor/lib64/hw/mapper.lapktest.so" "$here/tests/mock_mapper.c"
}

build_hardware
build_lvp
install_etc
build_test
