#!/usr/bin/env bash
# Build the session's generated pieces into out/session/:
#   overlay/system/lib64/liblinuxapk_shim.so   (NDK; LD_PRELOADed into init)
#   overlay/vendor/lib64/liblinuxapk_shim.so
#   overlay/<partition>/...                    image files with relaxed modes
#   pivot-exec                                 (static; pivots and execs init)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
out=${LINUXAPK_OUT:-$repo/out}
img=$out/img

ndk=${ANDROID_NDK_HOME:-}
if [[ -z $ndk ]]; then
  ndk=$(ls -d "$HOME"/Android/Sdk/ndk/* 2>/dev/null | sort -V | tail -1)
fi
cc="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang"
[[ -x $cc ]] || { echo "NDK clang not found (set ANDROID_NDK_HOME)" >&2; exit 1; }

rm -rf "$out/session/overlay"
mkdir -p "$out/session/overlay/system/lib64" "$out/session/overlay/vendor/lib64"
so="$out/session/overlay/system/lib64/liblinuxapk_shim.so"
"$cc" -shared -fPIC -O2 -Wall -Werror -fvisibility=hidden -Wl,-soname,liblinuxapk_shim.so \
  -o "$so" "$here/shim/linuxapk_shim.c"
cp "$so" "$out/session/overlay/vendor/lib64/"
"$cc" -static -O2 -Wall -Werror -o "$out/session/pivot-exec" "$here/helper/pivot-exec.c" "$here/helper/bpfemu.c"

# The extracted image lost its fs_config ownership: every file is owned by
# the invoking user (root in the session), so files meant for e.g.
# "0550 logd logd" are unusable by their real owner. Copy such files into
# the overlay with world read/exec bits added.
relax() {
  local part=$1 src=$2
  [[ -d $src ]] || return 0
  (cd "$src" && find . -type f \( ! -perm -o+r -o \( -perm -u+x ! -perm -o+x \) \) -print0) |
  while IFS= read -r -d '' f; do
    mkdir -p "$out/session/overlay/$part/$(dirname "$f")"
    cp -p "$src/$f" "$out/session/overlay/$part/$f"
    chmod o+r,g+r "$out/session/overlay/$part/$f"
    if [[ -x $src/$f ]]; then chmod go+x "$out/session/overlay/$part/$f"; fi
  done
}
relax system "$img/system/system"
relax vendor "$img/vendor"
relax system_ext "$img/system_ext"
relax product "$img/product"
relax odm "$img/vendor/odm"
echo "built $out/session"
