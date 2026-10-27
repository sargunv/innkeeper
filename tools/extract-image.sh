#!/usr/bin/env bash
# Extract an AOSP CI "-img-<build>.zip" (as downloaded by tools/fetch-image.sh)
# into plain directory trees:
#   <out>/{system,system_ext,product,vendor,odm,*_dlkm}
# The zip's sparse super.img holds the dynamic partitions (A/B: the _a slot is
# taken; the _b slot is empty or system_other).
# Unprivileged: runs inside the host-tools container (tools/hosttools.sh).
set -euo pipefail
if [[ ! -e /run/.containerenv ]]; then
  exec "$(dirname "$0")/hosttools.sh" "$0" "$@"
fi

usage="usage: extract-image.sh <ci-img.zip> <out-dir>"
src=${1:?$usage}
out=${2:?$usage}
work="$out/.work"
rm -rf "$work"
mkdir -p "$work"

fs_extract() {
  local img=$1 dest=$2
  rm -rf "$dest"
  mkdir -p "$dest"
  if [[ $(file -b "$img") == *ext4* || $(file -b "$img") == *ext2* ]]; then
    debugfs -R "rdump / $dest" "$img" >/dev/null 2>&1
  elif [[ $(file -b "$img") == *EROFS* ]] || head -c 1028 "$img" | tail -c 4 | od -An -tx4 | grep -q e0f5e1e2; then
    fsck.erofs --extract="$dest" --no-preserve "$img" >/dev/null
  else
    echo "unknown fs in $img: $(file -b "$img")" >&2
    return 1
  fi
  rm -rf "$dest/lost+found"
}

python3 -Ic "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2], ['super.img'])" "$src" "$work"
simg2img "$work/super.img" "$work/super.raw"
rm "$work/super.img"
mkdir -p "$work/parts"
lpunpack "$work/super.raw" "$work/parts/" >/dev/null
rm "$work/super.raw"
for p in "$work"/parts/*_a.img; do
  [[ $(stat -c %s "$p") -eq 0 ]] && continue
  name=$(basename "$p" _a.img)
  echo "extracting $name"
  fs_extract "$p" "$out/$name"
  rm "$p"
done
rm -rf "$work"
echo "done: $(ls "$out")"
