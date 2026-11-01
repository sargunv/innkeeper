#!/usr/bin/env bash
# build:vulkan cross-builds the vendor Vulkan drivers (Mesa RADV, ANV and
# lavapipe) for Android x86_64, and RADV and ANV for OpenHarmony x86_64, with
# the pinned NDK in the build container (tools/container/run.sh). It installs
# these into out/build/vulkan/install:
#   share/innkeeper/android/vendor/lib64/hw/vulkan.{radv,anv,lvp}.so
#   share/innkeeper/android/vendor/etc/libdrm/amdgpu.ids
#   share/innkeeper/props.d/vulkan.prop
#   share/innkeeper/ohos/vendor/lib64/chipsetsdk/libvulkan_{radeon,intel}.so
#   share/innkeeper/ohos/vendor/etc/vulkan/icd.d/{radeon,intel}_icd.x86_64.json
#   share/innkeeper/ohos/vendor/etc/libdrm/amdgpu.ids
# It also builds the test programs that tests/run.sh runs
# (out/build/vulkan/{test,test-vendor}). The build is skipped when its inputs
# (this directory, the container and the toolchain pin) still hash to
# out/build/vulkan/.stamp.
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
vendor=$work/install/share/innkeeper/android/vendor
rm -rf "$work/install"
mkdir -p "$src" "$build" "$deps" "$vendor"

# Generated cross file with the absolute NDK path, used together with
# cross/android-x86_64.ini.
cat >"$build/ndk.ini" <<EOF
[constants]
ndk_bin = '$ndk_bin'
api = '$API'
EOF
cross=(--cross-file "$build/ndk.ini" --cross-file "$here/cross/android-x86_64.ini")

export PKG_CONFIG_LIBDIR=$deps/vendor/lib64/pkgconfig
export PKG_CONFIG_SYSROOT_DIR=$deps
unset PKG_CONFIG_PATH

# fetch <dir> <url> <tag> shallow-clones a pinned tag and applies
# patches/<dir>/*.patch. It resets the tree only when the tag or patches change.
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
  # libdrm installs amdgpu.ids to <datadir>/libdrm, and libdrm_amdgpu has that
  # path baked in.
  install -Dm644 "$deps/vendor/etc/libdrm/amdgpu.ids" "$vendor/etc/libdrm/amdgpu.ids"
}

# Mesa's shader cache needs zlib. Link the NDK's static libz so the driver does
# not need libz.so, which is not LL-NDK.
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

# ANV compiles internal OpenCL shaders at build time. These native tools, and
# the LLVM and Clang they use, stay in the container and are not linked into
# the Android hardware HALs.
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
  python3 "$here/anv-pci-ids.py" "$src/mesa" >"$work/install/share/innkeeper/anv-pci-ids"
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
    -DCMAKE_C_COMPILER_LAUNCHER=sccache -DCMAKE_CXX_COMPILER_LAUNCHER=sccache \
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

# install_driver <built .so> <name> installs the driver as
# /vendor/lib64/hw/vulkan.<name>.so.
install_driver() {
  local so=$1 name=$2 dst=$build/vulkan.$2.so
  install -d "$vendor/lib64/hw"
  # libvulkan dlopens vulkan.<ro.hardware.vulkan>.so, so give the driver a
  # matching soname.
  "$ndk_bin/llvm-objcopy" --strip-unneeded "$so" "$dst"
  patchelf --set-soname "vulkan.$name.so" --remove-rpath "$dst"
  install -m644 "$dst" "$vendor/lib64/hw/vulkan.$name.so"
  check_elf "$vendor/lib64/hw/vulkan.$name.so"
}

# Vendor libraries may only need LL-NDK libraries and what we ship in
# /vendor/lib64.
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
  # A HAL module should export only HMI, with no symbols from the static LLVM
  # or libc++.
  local exports
  exports=$(readelf -sW --dyn-syms "$f" | awk '$7 != "UND" && ($5 == "GLOBAL" || $5 == "WEAK") {print $8}' | sort -u)
  if [[ $exports != HMI ]]; then
    echo "check_elf: $f exports $(echo "$exports" | wc -l) symbols, expected only HMI" >&2
    bad=1
  fi
  echo "check_elf: $(basename "$f") NEEDED: ${needed//$'\n'/ }; exports: $(echo "$exports" | cut -c1-80)"
  return $bad
}

# Graphics properties for the launcher. The feature XMLs are written at start.
install_etc() {
  install -D -m644 -t "$work/install/share/innkeeper/props.d" "$here/etc/vulkan.prop"
}

# Test programs for tests/run.sh go in $work/test, which is bound at
# /data/nativetest64, and a test-only vendor layer goes in $work/test-vendor.
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
  # Buffers from the allocator's code, for vkdriver_test --anb-hal.
  local graphics=$repo/android/vendor/graphics
  "$cc++" -O2 -std=c++20 -shared -fPIC -fvisibility=hidden -static-libstdc++ \
    -I"$graphics/common/include" -o "$t/libhalbuf.so" "$here/tests/halbuf.cpp" \
    "$graphics"/common/src/{buffer_alloc,layout,native_handle}.cpp -llog
  # The mock IMapper v5.
  "$cc" -O2 -Wall -shared -fPIC -fvisibility=hidden -o "$work/test-vendor/lib64/hw/mapper.inkptest.so" "$here/tests/mock_mapper.c"
}

# ---- OpenHarmony ------------------------------------------------------------
# OpenHarmony's processes run on the image's musl. These drivers build with the
# NDK's clang against the container's musl, and they link compiler-rt, libc++abi
# and libc++ statically. build_musl_runtimes builds those from the pinned LLVM
# source.
ohos_vendor=$work/install/share/innkeeper/ohos/vendor
musl=$build/musl
ohos_deps=$work/deps-ohos

build_musl_runtimes() {
  local sys=$musl/sysroot res=$musl/resource
  fetch llvm-project "$LLVM_URL" "$LLVM_TAG"
  rm -rf "$musl"
  mkdir -p "$sys/usr/lib" "$res"
  cp -r /usr/x86_64-linux-musl/include "$sys/usr/include"
  # musl's package has no kernel headers.
  cp -r /usr/include/{linux,asm,asm-generic} "$sys/usr/include/"
  cp -rL /usr/x86_64-linux-musl/lib64/. "$sys/usr/lib/"
  # OpenHarmony's libraries need musl as libc.so.
  patchelf --set-soname libc.so "$sys/usr/lib/libc.so"
  cp -r "$("$ndk_bin/clang" -print-resource-dir)/include" "$res/include"
  local flags="-resource-dir=$res" t=x86_64-linux-musl b=$build/musl-runtimes
  rm -rf "$b"
  cmake -GNinja -S "$src/llvm-project/runtimes" -B "$b" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_SYSROOT="$sys" -DCMAKE_INSTALL_PREFIX="$sys/usr" \
    -DCMAKE_C_COMPILER="$ndk_bin/clang" -DCMAKE_CXX_COMPILER="$ndk_bin/clang++" \
    -DCMAKE_ASM_COMPILER="$ndk_bin/clang" -DCMAKE_AR="$ndk_bin/llvm-ar" \
    -DCMAKE_RANLIB="$ndk_bin/llvm-ranlib" -DCMAKE_C_COMPILER_TARGET=$t \
    -DCMAKE_CXX_COMPILER_TARGET=$t -DCMAKE_ASM_COMPILER_TARGET=$t \
    -DCMAKE_C_COMPILER_LAUNCHER=sccache -DCMAKE_CXX_COMPILER_LAUNCHER=sccache \
    -DCMAKE_C_FLAGS="$flags" -DCMAKE_CXX_FLAGS="$flags" -DCMAKE_ASM_FLAGS="$flags" \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DLLVM_ENABLE_RUNTIMES="compiler-rt;libcxxabi;libcxx" -DLLVM_DEFAULT_TARGET_TRIPLE=$t \
    -DLLVM_ENABLE_PER_TARGET_RUNTIME_DIR=OFF -DLLVM_INCLUDE_TESTS=OFF \
    -DCOMPILER_RT_INSTALL_PATH="$res" -DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON \
    -DCOMPILER_RT_BUILD_BUILTINS=ON -DCOMPILER_RT_BUILD_CRT=ON \
    -DCOMPILER_RT_BUILD_SANITIZERS=OFF -DCOMPILER_RT_BUILD_XRAY=OFF \
    -DCOMPILER_RT_BUILD_LIBFUZZER=OFF -DCOMPILER_RT_BUILD_PROFILE=OFF \
    -DCOMPILER_RT_BUILD_MEMPROF=OFF -DCOMPILER_RT_BUILD_ORC=OFF \
    -DCOMPILER_RT_BUILD_CTX_PROFILE=OFF -DCOMPILER_RT_INCLUDE_TESTS=OFF \
    -DLIBCXX_HAS_MUSL_LIBC=ON -DLIBCXX_ENABLE_SHARED=OFF -DLIBCXXABI_ENABLE_SHARED=OFF \
    -DLIBCXX_ENABLE_STATIC_ABI_LIBRARY=ON -DLIBCXX_HERMETIC_STATIC_LIBRARY=ON \
    -DLIBCXXABI_HERMETIC_STATIC_LIBRARY=ON -DLIBCXX_ENABLE_EXCEPTIONS=OFF \
    -DLIBCXXABI_ENABLE_EXCEPTIONS=OFF -DLIBCXXABI_USE_LLVM_UNWINDER=OFF \
    -DLIBCXX_USE_COMPILER_RT=ON -DLIBCXXABI_USE_COMPILER_RT=ON \
    -DLIBCXX_INCLUDE_BENCHMARKS=OFF -DLIBCXX_INCLUDE_TESTS=OFF -DLIBCXXABI_INCLUDE_TESTS=OFF
  ninja -C "$b" -j "$JOBS" install
}

ohos_meson() {
  PKG_CONFIG_LIBDIR=$ohos_deps/vendor/lib64/pkgconfig PKG_CONFIG_SYSROOT_DIR=$ohos_deps \
    meson "$@"
}

build_libdrm_ohos() {
  fetch libdrm "$LIBDRM_URL" "$LIBDRM_TAG"
  local b=$build/libdrm-ohos
  # shellcheck disable=SC2046 # reconf prints zero or one flag
  ohos_meson setup $(reconf "$b") "$b" "$src/libdrm" "${ohos_cross[@]}" \
    --prefix=/vendor --libdir=lib64 --datadir=etc \
    -Ddefault_library=static -Dbuildtype=release \
    -Damdgpu=enabled -Dintel=disabled -Dradeon=disabled -Dnouveau=disabled \
    -Dvmwgfx=disabled -Dfreedreno=disabled -Dvc4=disabled -Detnaviv=disabled \
    -Dexynos=disabled -Domap=disabled -Dtegra=disabled -Dcairo-tests=disabled \
    -Dvalgrind=disabled -Dman-pages=disabled -Dtests=false -Dinstall-test-programs=false
  meson compile -C "$b" -j "$JOBS"
  DESTDIR=$ohos_deps meson install -C "$b" --quiet
  install -Dm644 "$ohos_deps/vendor/etc/libdrm/amdgpu.ids" "$ohos_vendor/etc/libdrm/amdgpu.ids"
}

build_ohos() {
  build_musl_runtimes
  cat >"$build/ohos.ini" <<EOF
[constants]
ndk_bin = '$ndk_bin'
sysroot = '$musl/sysroot'
resource = '$musl/resource'
EOF
  ohos_cross=(--cross-file "$build/ohos.ini" --cross-file "$here/cross/ohos-x86_64.ini")
  build_shader_tools
  build_libdrm_ohos
  fetch mesa "$MESA_URL" "$MESA_TAG"
  local b=$build/mesa-ohos
  # shellcheck disable=SC2046 # reconf prints zero or one flag
  ohos_meson setup $(reconf "$b") "$b" "$src/mesa" "${ohos_cross[@]}" \
    --prefix=/vendor --libdir=lib64/chipsetsdk \
    -Dbuildtype=release -Db_ndebug=true -Dcpp_eh=none \
    -Dplatforms=ohos -Dvulkan-drivers=amd,intel -Dgallium-drivers= \
    -Dllvm=disabled -Damd-use-llvm=false -Dmesa-clc=system -Dintel-rt=disabled \
    -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dopengl=false \
    -Dgles1=disabled -Dgles2=disabled -Dshader-cache=disabled \
    -Dzlib=disabled -Dzstd=disabled -Dexpat=disabled -Dxmlconfig=disabled \
    -Dvalgrind=disabled -Dlibunwind=disabled -Dlmsensors=disabled \
    -Dperfetto=false -Dvulkan-layers= -Dtools= -Dbuild-tests=false \
    -Dvideo-codecs=all_free -Dspirv-tools=disabled
  meson compile -C "$b" -j "$JOBS"
  local name
  for name in amd/vulkan/libvulkan_radeon intel/vulkan/libvulkan_intel; do
    local so=$ohos_vendor/lib64/chipsetsdk/${name##*/}.so
    install -d "${so%/*}"
    "$ndk_bin/llvm-objcopy" --strip-unneeded "$b/src/$name.so" "$so"
    chmod 644 "$so"
    check_elf_ohos "$so"
  done
  # The loader reads ICD manifests from /vendor/etc/vulkan/icd.d. Mesa's
  # manifests name each driver by its absolute path.
  install -D -m644 -t "$ohos_vendor/etc/vulkan/icd.d" \
    "$b/src/amd/vulkan/radeon_icd.x86_64.json" "$b/src/intel/vulkan/intel_icd.x86_64.json"
}

# OpenHarmony's drivers may only need musl, and they export only the ICD
# entry points.
check_elf_ohos() {
  local f=$1 needed exports
  needed=$(readelf -dW "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
  exports=$(readelf -sW --dyn-syms "$f" | awk '$7 != "UND" && ($5 == "GLOBAL" || $5 == "WEAK") {print $8}' | sort -u | paste -sd' ')
  echo "check_elf_ohos: $(basename "$f") NEEDED: ${needed//$'\n'/ }; exports: $exports"
  if [[ $needed != libc.so ]]; then
    echo "check_elf_ohos: $f: needs more than libc.so" >&2
    return 1
  fi
  if [[ $exports != "vk_icdGetInstanceProcAddr vk_icdGetPhysicalDeviceProcAddr vk_icdNegotiateLoaderICDInterfaceVersion" ]]; then
    echo "check_elf_ohos: $f exports more than the ICD entry points" >&2
    return 1
  fi
}

build_hardware
build_lvp
install_etc
build_test
build_ohos
