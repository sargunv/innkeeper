#!/usr/bin/env bash
# Refresh android/vendor/audio/third_party/aosp from an AOSP checkout (default
# ~/aosp/a17): tinyalsa headers and frozen audio AIDL APIs. Read-only use of the tree.
set -euo pipefail
AOSP=${AOSP:-$HOME/aosp/a17}
here=$(cd "$(dirname "$0")/.." && pwd)
dst=$here/third_party/aosp
rm -rf "$dst"
mkdir -p "$dst/include/tinyalsa"
ta=$AOSP/external/tinyalsa_new
for h in asoundlib.h attributes.h interval.h limits.h mixer.h pcm.h version.h; do
  cp "$ta/include/tinyalsa/$h" "$dst/include/tinyalsa/"
done
cp "$ta/NOTICE" "$dst/NOTICE.tinyalsa"
hi=$AOSP/hardware/interfaces
media=$AOSP/system/hardware/interfaces/media/aidl_api
pkgs=(
  "android.hardware.audio.core 4 $hi/audio/aidl/aidl_api"
  "android.hardware.audio.common 5 $hi/audio/aidl/aidl_api"
  "android.hardware.audio.core.sounddose 4 $hi/audio/aidl/aidl_api"
  "android.hardware.audio.effect 4 $hi/audio/aidl/aidl_api"
  "android.hardware.common.fmq 1 $hi/common/fmq/aidl/aidl_api"
  "android.hardware.common 2 $hi/common/aidl/aidl_api"
  "android.media.audio.common.types 5 $media"
  "android.media.audio.eraser.types 1 $media"
)
for entry in "${pkgs[@]}"; do
  read -r pkg version source <<<"$entry"
  mkdir -p "$dst/aidl/$pkg"
  cp -r "$source/$pkg/$version" "$dst/aidl/$pkg/"
done
cat >"$dst/SOURCE" <<S
Copied from AOSP refs/tags/android-17.0.0_r1 by android/vendor/audio/tools/sync-aosp.sh:
include/tinyalsa (external/tinyalsa_new, BSD-3-Clause, see NOTICE.tinyalsa);
aidl (hardware/interfaces and system/hardware/interfaces, Apache-2.0).
S
