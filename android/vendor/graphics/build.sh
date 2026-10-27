#!/usr/bin/env bash
# Build the display HALs out of tree with the NDK and install them into the
# vendor overlay, then run the host unit tests and the on-device tests
# (gralloc_test, hwc_test) through tests/bionic/run.sh.
#
#   android/vendor/graphics/build.sh            build + install + tests
#   android/vendor/graphics/build.sh --no-test  skip the tests
#
# Needs cmake, ninja, clang and python3 on the host.
# Env: NDK, AIDL, LINUXAPK_OUT (default: <main checkout>/out; installs into
#      its vendor-overlay, builds in its hal-build).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
main=$(cd "$(git -C "$repo" rev-parse --git-common-dir)/.." && pwd)
export LINUXAPK_OUT=${LINUXAPK_OUT:-$main/out}
OVERLAY=$LINUXAPK_OUT/vendor-overlay
BUILD=$LINUXAPK_OUT/hal-build
run_tests=1
[[ "${1:-}" == "--no-test" ]] && run_tests=0

NDK=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}
export AIDL=${AIDL:-$HOME/Android/Sdk/build-tools/37.0.0/aidl}

"$here/tools/gen-aidl.sh" "$BUILD/aidl" >/dev/null

echo "== host"
cmake -S "$here" -B "$BUILD/host" -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++ \
  -DAIDL_GEN_DIR="$BUILD/aidl" >/dev/null
cmake --build "$BUILD/host"
if ((run_tests)); then
  (cd "$BUILD/host" && ctest --output-on-failure)
fi

echo "== android x86_64"
cmake -S "$here" -B "$BUILD/android" -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
  -DANDROID_ABI=x86_64 -DANDROID_PLATFORM=android-35 -DANDROID_STL=c++_static \
  -DAIDL_GEN_DIR="$BUILD/aidl" >/dev/null
cmake --build "$BUILD/android"

echo "== install -> $OVERLAY"
b=$BUILD/android
strip=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip
inst() {
  install -D -m "$1" "$2" "$3"
  [[ "$1" == 755 || "$3" == *.so ]] && "$strip" --strip-unneeded "$3" || true
}
inst 755 "$b/android.hardware.graphics.allocator-service.linuxapk" "$OVERLAY/bin/hw/android.hardware.graphics.allocator-service.linuxapk"
inst 755 "$b/android.hardware.graphics.composer3-service.linuxapk" "$OVERLAY/bin/hw/android.hardware.graphics.composer3-service.linuxapk"
inst 644 "$b/mapper.linuxapk.so" "$OVERLAY/lib64/hw/mapper.linuxapk.so"
for f in "$here"/etc/init/*.rc; do inst 644 "$f" "$OVERLAY/etc/init/$(basename "$f")"; done
for f in "$here"/etc/vintf/*.xml; do inst 644 "$f" "$OVERLAY/etc/vintf/manifest/$(basename "$f")"; done
(cd "$OVERLAY" && find . -type f | sort | sed 's/^/  /')
# Test binaries go next to the overlay, not into it.
mkdir -p "$BUILD/test-vendor/bin"
cp "$b/gralloc_test" "$b/hwc_test" "$BUILD/test-vendor/bin/"
if ((run_tests)); then
  # The test binaries run from a /vendor layer above the overlay.
  echo "== android tests"
  run=$repo/tests/bionic/run.sh
  "$run" -v "$BUILD/test-vendor" -- /vendor/bin/gralloc_test
  "$run" -v "$BUILD/test-vendor" -e LINUXAPK_PROPS=vendor.linuxapk.hwc.dump.dir=/data/vendor/linuxapk/frames \
    -e EXPECT_DUMP=/data/vendor/linuxapk/frames -- /vendor/bin/hwc_test
fi
