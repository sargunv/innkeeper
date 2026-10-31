#!/usr/bin/env bash
# Generate NDK-backend C++ for frozen AIDL snapshots in
# android/vendor/third_party/aosp/aidl, for the packages a HAL implements.
# usage: gen-aidl.sh OUT_DIR PACKAGE...   (writes OUT_DIR/include and OUT_DIR/src)
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)/third_party/aosp/aidl
AIDL=$ANDROID_HOME/build-tools/$ANDROID_BUILD_TOOLS/aidl
out=$1
shift
# Generate into a scratch dir, then replace only changed files: every configure
# runs this, and fresh mtimes would recompile every HAL.
gen=$out/.gen
rm -rf "$gen"
mkdir -p "$gen/include" "$gen/src"
incs=()
for d in "$root"/*/*/; do incs+=(-I "$d"); done
for pkg in "$@"; do
  for pkgdir in "$root/$pkg"/*/; do
    ver=$(basename "$pkgdir")
    hash=$(cat "$pkgdir/.hash" 2>/dev/null | tail -1 || echo notfrozen)
    mapfile -t files < <(find "$pkgdir" -name '*.aidl' | sort)
    "$AIDL" --lang=ndk --structured --stability=vintf --min_sdk_version=34 \
      --version="$ver" --hash="$hash" "${incs[@]}" \
      -o "$gen/src" -h "$gen/include" "${files[@]}"
  done
done
for d in include src; do
  mkdir -p "$out/$d"
  while IFS= read -r f; do
    cmp -s "$gen/$d/$f" "$out/$d/$f" || install -D -m 644 "$gen/$d/$f" "$out/$d/$f"
  done < <(cd "$gen/$d" && find . -type f -printf '%P\n')
  while IFS= read -r f; do
    [[ -e $gen/$d/$f ]] || rm "$out/$d/$f"
  done < <(cd "$out/$d" && find . -type f -printf '%P\n')
done
rm -rf "$gen"
echo "generated $(find "$out/src" -name '*.cpp' | wc -l) sources"
