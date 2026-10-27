#!/usr/bin/env bash
# Refresh audio/third_party/aosp/include from an AOSP checkout (default
# ~/aosp/a17): the legacy audio HAL headers (hardware/audio.h and what it
# includes), all Apache-2.0. Read-only use of the tree; nothing is built.
set -euo pipefail
AOSP=${AOSP:-$HOME/aosp/a17}
here=$(cd "$(dirname "$0")/.." && pwd)
dst=$here/third_party/aosp/include
rm -rf "$dst"
mkdir -p "$dst"
files=(
  hardware/libhardware/include:hardware/audio.h
  hardware/libhardware/include:hardware/audio_effect.h
  hardware/libhardware/include:hardware/hardware.h
  system/core/libcutils/include:cutils/bitops.h
  system/core/libcutils/include:cutils/native_handle.h
  system/core/libsystem/include:system/graphics.h
  system/core/libsystem/include:system/graphics-base.h
  system/core/libsystem/include:system/graphics-base-v1.0.h
  system/core/libsystem/include:system/graphics-base-v1.1.h
  system/core/libsystem/include:system/graphics-base-v1.2.h
  system/core/libsystem/include:system/graphics-sw.h
  system/media/audio/include:system/audio.h
  system/media/audio/include:system/audio-base.h
  system/media/audio/include:system/audio-base-utils.h
  system/media/audio/include:system/audio-hal-enums.h
  system/media/audio/include:system/audio_common-base.h
  system/media/audio/include:system/audio_effect.h
  system/media/audio/include:system/audio_effect-base.h
)
for f in "${files[@]}"; do
  mkdir -p "$dst/$(dirname "${f#*:}")"
  cp "$AOSP/${f%%:*}/${f#*:}" "$dst/${f#*:}"
done
cat >"$here/third_party/aosp/SOURCE" <<S
Copied from AOSP refs/tags/android-17.0.0_r1 by audio/tools/sync-aosp.sh.
Apache-2.0; see the license headers in each file.
S
