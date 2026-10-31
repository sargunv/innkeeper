#!/usr/bin/env bash
# fetch.sh's hook for Android: an AOSP CI "-img-<build>.zip"
# into plain directory trees:
#   <out>/{system,system_ext,product,vendor,odm,*_dlkm}
# The zip's sparse super.img holds the dynamic partitions (A/B: the _a slot is
# taken; the _b slot is empty or system_other). Unprivileged, with the static
# tools from image/tools.
#
# usage: extract-android.sh <ci-img.zip> <out-dir>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tools=$(dirname "$here")
usage="usage: extract-android.sh <ci-img.zip> <out-dir>"
src=${1:?$usage}
out=${2:?$usage}
work="$out/.work"
rm -rf "$work"
mkdir -p "$work"

python3 -Ic "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2], ['super.img'])" "$src" "$work"
"$tools/simg2img" "$work/super.img" "$work/super.raw"
rm "$work/super.img"
mkdir -p "$work/parts"
"$tools/lpunpack" "$work/super.raw" "$work/parts/" >/dev/null
rm "$work/super.raw"
for p in "$work"/parts/*_a.img; do
  [[ $(stat -c %s "$p") -eq 0 ]] && continue
  name=$(basename "$p" _a.img)
  echo "extracting $name"
  rm -rf "${out:?}/$name"
  "$here/fs-extract.sh" "$p" "$out/$name"
  rm "$p"
done
rm -rf "$work"
echo "done: $(ls "$out")"
