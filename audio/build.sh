#!/usr/bin/env bash
# Build the Android side of Linux.apk audio with the NDK:
#   libtinyalsav2.so  tinyalsa API backed by the host bridge (tinyalsa/), for
#                     the image's audio HAL; see docs/audio.md
#   linuxapk-tone     AAudio tone + latency test (tests/tone.c), not installed
# and install into the vendor overlay.
#
# Env: LINUXAPK_OUT (default <repo>/out), NDK.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
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
"$cc" "${cflags[@]}" -fPIE -pie -o "$build/linuxapk-tone" "$here/tests/tone.c" -laaudio -lm

# Every symbol the image's libtinyalsav2.so exports must exist in ours.
if [[ -f $out/img/vendor/lib64/libtinyalsav2.so ]]; then
  missing=$(comm -23 \
    <("$bin/llvm-nm" -D --defined-only "$out/img/vendor/lib64/libtinyalsav2.so" | awk '$2 == "T" {print $3}' | sort -u) \
    <("$bin/llvm-nm" -D --defined-only "$build/libtinyalsav2.so" | awk '$2 == "T" {print $3}' | sort -u))
  if [[ -n $missing ]]; then
    echo "libtinyalsav2.so lacks image symbols:" "$missing" >&2
    exit 1
  fi
fi

ovl=$out/vendor-overlay
install -D -m 644 "$build/libtinyalsav2.so" "$ovl/lib64/libtinyalsav2.so"
"$bin/llvm-strip" --strip-unneeded "$ovl/lib64/libtinyalsav2.so"
install -D -m 644 "$here/etc/audio.prop" "$ovl/etc/audio.prop"
echo "built $build; installed into $ovl"
