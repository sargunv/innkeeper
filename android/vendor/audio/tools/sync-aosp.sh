#!/usr/bin/env bash
# Refresh android/vendor/audio/third_party/aosp from an AOSP checkout (default
# ~/aosp/a17): tinyalsa's public headers (BSD, external/tinyalsa_new), which our
# libtinyalsav2.so replacement implements. Read-only use of the tree.
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
cat >"$dst/SOURCE" <<S
Copied from AOSP refs/tags/android-17.0.0_r1 by android/vendor/audio/tools/sync-aosp.sh:
include/tinyalsa (external/tinyalsa_new, BSD-3-Clause, see NOTICE.tinyalsa).
S
