#!/usr/bin/env bash
# Build the Android side of Linux.apk audio with the NDK:
#   libtinyalsav2.so  tinyalsa API backed by the host bridge (tinyalsa/), for
#                     the image's audio HAL; see docs/audio.md
#   linuxapk-tone     AAudio tone + latency test (tests/e2e/audio/tone.c), not
#                     installed
# libtinyalsav2.so and the AIDL audio HAL's rc override go into the HAL's APEX
# (out/apex-overlay); audio.prop and the policy XML go to out/vendor-overlay/etc.
#
# Env: LINUXAPK_OUT (default <repo>/out), NDK.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
out=${LINUXAPK_OUT:-$repo/out}
ndk=${NDK:-$(printf '%s\n' "$HOME"/Android/Sdk/ndk/* | sort -V | tail -1)}
bin=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
cc=$bin/x86_64-linux-android30-clang
[[ -x $cc ]] || {
  echo "NDK clang not found (set NDK)" >&2
  exit 1
}
build=$out/audio-build
mkdir -p "$build"
cflags=(-O2 -g -Wall -Wextra -Werror -std=c17 -fPIC -I"$here/third_party/aosp/include")

"$cc" "${cflags[@]}" -shared -fvisibility=hidden -Wl,-soname,libtinyalsav2.so \
  -Wl,--version-script="$here/tinyalsa/exports.map" -Wl,-z,defs \
  -o "$build/libtinyalsav2.so" \
  "$here/tinyalsa/pcm.c" "$here/tinyalsa/mixer.c" "$here/tinyalsa/bridge_client.c" \
  "$here/tinyalsa/internal.c" -llog
"$cc" "${cflags[@]}" -fPIE -pie -o "$build/linuxapk-tone" "$repo/tests/e2e/audio/tone.c" -laaudio -lm
# The image's own libtinyalsav2.so, in the AIDL audio HAL's APEX: ours
# replaces it, so it must export everything the image's does.
apex=$("$repo/host/launcher/linuxapk-session" image | sed -n 's/^apex=//p')
image_lib=$apex/com.android.hardware.audio/lib64
[[ -f $image_lib/libtinyalsav2.so ]] || {
  echo "no $image_lib/libtinyalsav2.so (run host/launcher/linuxapk-session prepare)" >&2
  exit 1
}
missing=$(comm -23 \
  <("$bin/llvm-nm" -D --defined-only "$image_lib/libtinyalsav2.so" | awk '$2 == "T" {print $3}' | sort -u) \
  <("$bin/llvm-nm" -D --defined-only "$build/libtinyalsav2.so" | awk '$2 == "T" {print $3}' | sort -u))
if [[ -n $missing ]]; then
  echo "libtinyalsav2.so lacks image symbols:" "$missing" >&2
  exit 1
fi

hal_apex=$out/apex-overlay/com.android.hardware.audio
apex_lib=$hal_apex/lib64/libtinyalsav2.so
install -D -m 644 "$build/libtinyalsav2.so" "$apex_lib"
"$bin/llvm-strip" --strip-unneeded "$apex_lib"
rc=android.hardware.audio.service-aidl.example.rc
install -D -m 644 "$here/etc/apex/com.android.hardware.audio/etc/$rc" "$hal_apex/etc/$rc"
etc=$out/vendor-overlay/etc
install -D -m 644 "$here/etc/audio.prop" "$etc/audio.prop"
install -D -m 644 "$here/etc/primary_audio_policy_configuration.xml" "$etc/primary_audio_policy_configuration.xml"
echo "built $build; installed into $hal_apex and $etc"
