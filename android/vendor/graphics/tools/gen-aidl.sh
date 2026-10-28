#!/usr/bin/env bash
# Generate NDK-backend C++ for the frozen AIDL snapshots in third_party/aosp/aidl.
# usage: gen-aidl.sh OUT_DIR [AIDL_ROOT]   (writes OUT_DIR/include and OUT_DIR/src)
# AIDL_ROOT defaults to this module's third_party/aosp/aidl (the health HAL
# passes its own).
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
AIDL=$ANDROID_HOME/build-tools/$ANDROID_BUILD_TOOLS/aidl
out=$1
root=${2:-$here/third_party/aosp/aidl}
rm -rf "$out/include" "$out/src"
mkdir -p "$out/include" "$out/src"
incs=()
for d in "$root"/*/*/; do incs+=(-I "$d"); done
for pkgdir in "$root"/*/*/; do
  ver=$(basename "$pkgdir")
  hash=$(cat "$pkgdir/.hash" 2>/dev/null | tail -1 || echo notfrozen)
  mapfile -t files < <(find "$pkgdir" -name '*.aidl' | sort)
  "$AIDL" --lang=ndk --structured --stability=vintf --min_sdk_version=34 \
    --version="$ver" --hash="$hash" "${incs[@]}" \
    -o "$out/src" -h "$out/include" "${files[@]}"
done
echo "generated $(find "$out/src" -name '*.cpp' | wc -l) sources"
