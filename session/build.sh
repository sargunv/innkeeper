#!/usr/bin/env bash
# Build the session's generated pieces into out/session/:
#   overlay/system/lib64/liblinuxapk_shim.so   (NDK; LD_PRELOADed into init)
#   overlay/vendor/lib64/liblinuxapk_shim.so
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
out=${LINUXAPK_OUT:-$repo/out}

ndk=${ANDROID_NDK_HOME:-}
if [[ -z $ndk ]]; then
  ndk=$(ls -d "$HOME"/Android/Sdk/ndk/* 2>/dev/null | sort -V | tail -1)
fi
cc="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang"
[[ -x $cc ]] || { echo "NDK clang not found (set ANDROID_NDK_HOME)" >&2; exit 1; }

mkdir -p "$out/session/overlay/system/lib64" "$out/session/overlay/vendor/lib64"
so="$out/session/overlay/system/lib64/liblinuxapk_shim.so"
"$cc" -shared -fPIC -O2 -Wall -Werror -fvisibility=hidden -Wl,-soname,liblinuxapk_shim.so \
  -o "$so" "$here/shim/selinux_shim.c"
cp "$so" "$out/session/overlay/vendor/lib64/"
echo "built $so"
