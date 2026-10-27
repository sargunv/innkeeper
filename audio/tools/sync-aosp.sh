#!/usr/bin/env bash
# Refresh audio/third_party/aosp from an AOSP checkout (default ~/aosp/a17):
# - tinyalsa's public headers (BSD, external/tinyalsa_new), which our
#   libtinyalsav2.so replacement implements;
# - alsa_utils' headers and what they include (Apache-2.0), for
#   tests/alsa_proxy_test.c, which drives the image's own libalsautilsv2.
# Read-only use of the tree.
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
files=(
  system/media/alsa_utils/include:alsa_device_profile.h
  system/media/alsa_utils/include:alsa_device_proxy.h
  system/core/libcutils/include:cutils/bitops.h
  system/media/audio/include:system/audio.h
  system/media/audio/include:system/audio-base.h
  system/media/audio/include:system/audio-base-utils.h
  system/media/audio/include:system/audio-hal-enums.h
  system/media/audio/include:system/audio_common-base.h
)
for f in "${files[@]}"; do
  mkdir -p "$dst/include/$(dirname "${f#*:}")"
  cp "$AOSP/${f%%:*}/${f#*:}" "$dst/include/${f#*:}"
done
cat >"$dst/SOURCE" <<S
Copied from AOSP refs/tags/android-17.0.0_r1 by audio/tools/sync-aosp.sh:
include/tinyalsa (external/tinyalsa_new, BSD-3-Clause, see NOTICE.tinyalsa);
everything else Apache-2.0 (see the license headers in each file).
S
