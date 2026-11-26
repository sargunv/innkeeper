#!/usr/bin/env bash
# Refresh android/vendor/third_party/aosp from an AOSP checkout (default
# ~/aosp/a17): the frozen AIDL API snapshots the HALs implement, the binder NDK,
# IMapper and camera metadata headers, and the symbol maps of the binder NDK and
# libcamera_metadata. The script only reads the AOSP tree.
set -euo pipefail
AOSP=${AOSP:-$HOME/aosp/a17}
vendor=$(cd "$(dirname "$0")/.." && pwd)
dst=$vendor/third_party/aosp
hi=$AOSP/hardware/interfaces
media=$AOSP/system/hardware/interfaces/media/aidl_api
rm -rf "$dst"
mkdir -p "$dst/aidl" "$dst/include"

# package  version  its aidl_api dir
aidl_pkgs=(
  "android.hardware.common 2 $hi/common/aidl/aidl_api"
  "android.hardware.common.fmq 1 $hi/common/fmq/aidl/aidl_api"
  "android.hardware.drm.common 1 $hi/drm/common/aidl/aidl_api"
  "android.hardware.graphics.common 7 $hi/graphics/common/aidl/aidl_api"
  "android.hardware.graphics.allocator 3 $hi/graphics/allocator/aidl/aidl_api"
  "android.hardware.graphics.composer3 5 $hi/graphics/composer/aidl/aidl_api"
  "android.hardware.audio.core 4 $hi/audio/aidl/aidl_api"
  "android.hardware.audio.common 5 $hi/audio/aidl/aidl_api"
  "android.hardware.audio.core.sounddose 4 $hi/audio/aidl/aidl_api"
  "android.hardware.audio.effect 4 $hi/audio/aidl/aidl_api"
  "android.media.audio.common.types 5 $media"
  "android.media.audio.eraser.types 1 $media"
  "android.hardware.health 5 $hi/health/aidl/aidl_api"
  "android.hardware.camera.common 1 $hi/camera/common/aidl/aidl_api"
  "android.hardware.camera.device 1 $hi/camera/device/aidl/aidl_api"
  "android.hardware.camera.metadata 1 $hi/camera/metadata/aidl/aidl_api"
  "android.hardware.camera.provider 1 $hi/camera/provider/aidl/aidl_api"
)
for p in "${aidl_pkgs[@]}"; do
  read -r name ver dir <<<"$p"
  mkdir -p "$dst/aidl/$name"
  cp -r "$dir/$name/$ver" "$dst/aidl/$name/$ver"
done

bn=$AOSP/frameworks/native/libs/binder/ndk
for d in include_cpp include_ndk include_platform; do cp -r "$bn/$d/android/." "$dst/include/android/"; done
cp "$bn/libbinder_ndk.map.txt" "$dst/"

mp=$hi/graphics/mapper/stable-c
mkdir -p "$dst/include/android/hardware/graphics/mapper/utils"
cp "$mp/include/android/hardware/graphics/mapper/IMapper.h" "$dst/include/android/hardware/graphics/mapper/"
cp "$mp/implutils/include/android/hardware/graphics/mapper/utils/IMapperMetadataTypes.h" \
  "$dst/include/android/hardware/graphics/mapper/utils/"

# libcamera_metadata is LL-NDK. The camera HAL links a stub of its map.
cm=$AOSP/system/media/camera
mkdir -p "$dst/include/system" "$dst/include/cutils"
cp "$cm/include/system/"camera_{metadata,metadata_tags,vendor_tags}.h "$dst/include/system/"
cp "$cm/libcamera_metadata.map.txt" "$dst/"
cp "$AOSP/system/core/libcutils/include/cutils/compiler.h" "$dst/include/cutils/"

cat >"$dst/SOURCE" <<S
Copied from AOSP $(cd "$AOSP" && grep -o 'revision="[^"]*"' .repo/manifests/default.xml 2>/dev/null | head -1 || echo refs/tags/android-17.0.0_r1)
by android/vendor/tools/sync-aosp.sh. Apache-2.0; see the license headers in
each file.
S
find "$dst" -name Android.bp -delete
echo "synced into $dst"
