#!/usr/bin/env bash
# Derives the layer that the session stacks directly above the image
# (host/launcher/src/sandbox.rs) into <out-dir>. The extracted image has lost
# its fs_config ownership. Every file belongs to the invoking user, who is root
# in the session, so a file meant to be "0550 logd logd" is unusable by its
# real owner.
#   <partition>/  copies of image files with read/exec bits added.
#   apex/         the APEX files whose owner Android verifies (clatd), with
#                 their modes and the Android owner in the user.innkeeper.owner
#                 xattr that the broker and shim report (supervisor/README.md).
#
# usage: derive.sh <img-dir> <apex-dir> <out-dir>
set -euo pipefail
usage="usage: derive.sh <img-dir> <apex-dir> <out-dir>"
img=${1:?$usage} apex=${2:?$usage} out=${3:?$usage}

relax() {
  local src=$2 dst=$out/$1
  [[ -d $src ]] || return 0
  (cd "$src" && find . -type f \( ! -perm -o+r -o \( -perm -u+x ! -perm -o+x \) \) -print0) |
    while IFS= read -r -d '' f; do
      mkdir -p "$dst/$(dirname "$f")"
      cp -p "$src/$f" "$dst/$f"
      chmod o+r,g+r "$dst/$f"
      if [[ -x $src/$f ]]; then chmod go+x "$dst/$f"; fi
    done
}

#   path (relative to the flattened /apex)            mode   uid  gid
apex_fs_config=(
  "com.android.tethering/bin/for-system 0750 1029 1000"       # AID_CLAT:AID_SYSTEM
  "com.android.tethering/bin/for-system/clatd 6755 1029 1029" # ClatCoordinator checks
)
copy_apex_files() {
  local e rel
  for e in "${apex_fs_config[@]}"; do
    read -r rel _ <<<"$e"
    [[ -e $apex/$rel ]] || continue
    mkdir -p "$(dirname "$out/apex/$rel")"
    if [[ -d $apex/$rel ]]; then mkdir -p "$out/apex/$rel"; else cp -p "$apex/$rel" "$out/apex/$rel"; fi
  done
}

rm -rf "$out"
mkdir -p "$out"
relax system "$img/system/system"
relax vendor "$img/vendor"
relax system_ext "$img/system_ext"
relax product "$img/product"
relax odm "$img/odm"
copy_apex_files
for e in "${apex_fs_config[@]}"; do
  read -r rel mode uid gid <<<"$e"
  [[ -e $out/apex/$rel ]] || continue
  chmod "$mode" "$out/apex/$rel"
  python3 -Ic 'import os, sys; os.setxattr(sys.argv[1], "user.innkeeper.owner", sys.argv[2].encode())' \
    "$out/apex/$rel" "$uid:$gid"
done
