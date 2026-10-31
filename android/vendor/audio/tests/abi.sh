#!/usr/bin/env bash
# Our libtinyalsav2.so replaces the image's copy in the AIDL audio HAL's APEX,
# so it must export every function the image's does.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
stage=$repo/out/stage
ours=$stage/share/innkeeper/android/apex/com.android.hardware.audio/lib64/libtinyalsav2.so
[[ -f $ours ]] || {
  echo "no $ours (run \`mise run build\`)" >&2
  exit 1
}
apex=$("$stage/libexec/innkeeper/innkeeper-session" image | sed -n 's/^apex=//p')
theirs=$apex/com.android.hardware.audio/lib64/libtinyalsav2.so
[[ -f $theirs ]] || {
  echo "image not prepared: no $theirs (run \`mise run session -- prepare\`)" >&2
  exit 1
}
nm=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm
exports() { "$nm" -D --defined-only "$1" | awk '$2 == "T" {print $3}' | sort -u; }
missing=$(comm -23 <(exports "$theirs") <(exports "$ours"))
if [[ -n $missing ]]; then
  echo "FAIL libtinyalsav2.so lacks image symbols:" "${missing//$'\n'/ }" >&2
  exit 1
fi
echo "ok   libtinyalsav2.so exports everything the image's does"
