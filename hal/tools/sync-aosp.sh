#!/usr/bin/env bash
# Refresh hal/third_party/aosp from an AOSP checkout (default ~/aosp/a17).
# Copies only frozen AIDL API snapshots and a few headers, all Apache-2.0.
set -euo pipefail
AOSP=${AOSP:-$HOME/aosp/a17}
here=$(cd "$(dirname "$0")/.." && pwd)
dst=$here/third_party/aosp
hi=$AOSP/hardware/interfaces
rm -rf "$dst"
mkdir -p "$dst/aidl" "$dst/include"

# name  version  aidl_api dir
aidl_pkgs=(
  "android.hardware.common 2 $hi/common/aidl/aidl_api/android.hardware.common"
  "android.hardware.graphics.common 7 $hi/graphics/common/aidl/aidl_api/android.hardware.graphics.common"
  "android.hardware.drm.common 1 $hi/drm/common/aidl/aidl_api/android.hardware.drm.common"
  "android.hardware.graphics.allocator 3 $hi/graphics/allocator/aidl/aidl_api/android.hardware.graphics.allocator"
  "android.hardware.graphics.composer3 5 $hi/graphics/composer/aidl/aidl_api/android.hardware.graphics.composer3"
)
for p in "${aidl_pkgs[@]}"; do
  read -r name ver dir <<<"$p"
  mkdir -p "$dst/aidl/$name"
  cp -r "$dir/$ver" "$dst/aidl/$name/$ver"
done

bn=$AOSP/frameworks/native/libs/binder/ndk
for d in include_cpp include_ndk include_platform; do cp -r "$bn/$d/android/." "$dst/include/android/"; done
cp "$bn/libbinder_ndk.map.txt" "$dst/"

mp=$hi/graphics/mapper/stable-c
mkdir -p "$dst/include/android/hardware/graphics/mapper/utils"
cp "$mp/include/android/hardware/graphics/mapper/IMapper.h" "$dst/include/android/hardware/graphics/mapper/"
cp "$mp/implutils/include/android/hardware/graphics/mapper/utils/IMapperMetadataTypes.h" \
   "$dst/include/android/hardware/graphics/mapper/utils/"

cat >"$dst/SOURCE" <<S
Copied from AOSP $(cd "$AOSP" && cat .repo/manifests/default.xml 2>/dev/null | grep -o 'revision="[^"]*"' | head -1 || echo android-17.0.0_r1)
by hal/tools/sync-aosp.sh. Apache-2.0; see the license headers in each file.
S
find "$dst" -name Android.bp -delete
echo "synced into $dst"
