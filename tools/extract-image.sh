#!/usr/bin/env bash
# Extract an Android emulator system image (system.img + vendor.img + ramdisk.img)
# into plain directory trees: <out>/{system,system_ext,product,vendor,...,ramdisk}.
# Unprivileged: runs inside the host-tools container (tools/hosttools.sh).
set -euo pipefail
if [[ ! -e /run/.containerenv ]]; then
  exec "$(dirname "$0")/hosttools.sh" "$0" "$@"
fi

img_dir=${1:?usage: extract-image.sh <sdk-system-image-dir> <out-dir>}
out=${2:?usage: extract-image.sh <sdk-system-image-dir> <out-dir>}
work="$out/.work"
mkdir -p "$work"

# Copy partition $2 out of GPT disk image $1 into $3.
gpt_extract() {
  local disk=$1 name=$2 dest=$3
  read -r start size < <(sfdisk --json "$disk" | python3 -Ic "
import json,sys
for p in json.load(sys.stdin)['partitiontable']['partitions']:
    if p.get('name') == '$name': print(p['start'], p['size'])")
  dd if="$disk" of="$dest" bs=512 skip="$start" count="$size" status=none
}

fs_extract() {
  local img=$1 dest=$2
  rm -rf "$dest"; mkdir -p "$dest"
  if [[ $(file -b "$img") == *ext4* || $(file -b "$img") == *ext2* ]]; then
    debugfs -R "rdump / $dest" "$img" >/dev/null 2>&1
  elif [[ $(file -b "$img") == *EROFS* ]] || head -c 1028 "$img" | tail -c 4 | od -An -tx4 | grep -q e0f5e1e2; then
    fsck.erofs --extract="$dest" --no-preserve "$img" >/dev/null
  else
    echo "unknown fs in $img: $(file -b "$img")" >&2; return 1
  fi
}

gpt_extract "$img_dir/system.img" super "$work/super.img"
lpunpack "$work/super.img" "$work/" >/dev/null
gpt_extract "$img_dir/vendor.img" vendor "$work/vendor.img"

for p in "$work"/*.img; do
  name=$(basename "$p" .img)
  [[ $name == super ]] && continue
  [[ $(stat -c %s "$p") -eq 0 ]] && continue
  name=${name%_a}
  echo "extracting $name"
  fs_extract "$p" "$out/$name"
done

rm -rf "$out/ramdisk"; mkdir -p "$out/ramdisk"
lz4 -dc "$img_dir/ramdisk.img" | (cd "$out/ramdisk" && cpio -idm --quiet 2>/dev/null)
cp "$img_dir/build.prop" "$out/" 2>/dev/null || true
rm -rf "$work"
echo "done: $(ls "$out")"
