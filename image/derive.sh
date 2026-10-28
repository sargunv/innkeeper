#!/usr/bin/env bash
# Derive one of the layers the session stacks right above the image
# (ns-init.sh), into <out-dir>:
#   common       <partition>/: copies of image files with read/exec bits
#                added. The extracted image lost its fs_config ownership:
#                every file belongs to the invoking user (root in the
#                session), so a file meant for e.g. "0550 logd logd" is
#                unusable by its real owner.
#   subuid       apex/: APEX files whose fs_config owner and mode code verifies
#                at runtime, chowned to that owner inside "podman unshare",
#                whose id map (0 = us, 1.. = /etc/subuid) matches the
#                session's, so e.g. 1029 here is AID_CLAT.
#   single-uid   apex/: the same files for single-uid mode (docs/sandbox.md),
#                owned by us, with the Android owner in the
#                user.linuxapk.owner xattr that the shim reports.
#
# usage: derive.sh common|subuid|single-uid <img-dir> <apex-dir> <out-dir>
set -euo pipefail
usage="usage: derive.sh common|subuid|single-uid <img-dir> <apex-dir> <out-dir>"
layer=${1:?$usage} img=${2:?$usage} apex=${3:?$usage} out=${4:?$usage}

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

case $layer in
  common)
    rm -rf "$out"
    mkdir -p "$out"
    relax system "$img/system/system"
    relax vendor "$img/vendor"
    relax system_ext "$img/system_ext"
    relax product "$img/product"
    relax odm "$img/odm"
    ;;
  subuid)
    # Owned by subordinate ids: remove it from inside a matching user namespace.
    podman unshare rm -rf "$out"
    mkdir -p "$out"
    copy_apex_files
    # In reverse, so files are done before their (possibly 0750) parents.
    for ((i = ${#apex_fs_config[@]} - 1; i >= 0; i--)); do
      read -r rel mode uid gid <<<"${apex_fs_config[i]}"
      podman unshare sh -c '[ -e "$4" ] || exit 0; chown "$1:$2" "$4" && chmod "$3" "$4"' \
        _ "$uid" "$gid" "$mode" "$out/apex/$rel"
    done
    ;;
  single-uid)
    rm -rf "$out"
    mkdir -p "$out"
    copy_apex_files
    for e in "${apex_fs_config[@]}"; do
      read -r rel mode uid gid <<<"$e"
      [[ -e $out/apex/$rel ]] || continue
      chmod "$mode" "$out/apex/$rel"
      setfattr -n user.linuxapk.owner -v "$uid:$gid" "$out/apex/$rel"
    done
    ;;
  *)
    echo "$usage" >&2
    exit 1
    ;;
esac
