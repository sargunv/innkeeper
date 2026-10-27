#!/usr/bin/env bash
# Extract an Android image into plain directory trees:
#   <out>/{system,system_ext,product,vendor,odm,*_dlkm,ramdisk}
#
# Sources:
#   - an SDK emulator system-image dir (system.img + vendor.img GPT disks,
#     ramdisk.img), e.g. ~/Android/Sdk/system-images/android-37.0/default/x86_64;
#   - an AOSP CI "-img-<build>.zip" (sparse super.img + init_boot.img), as
#     downloaded by tools/fetch-image.sh.
# Unprivileged: runs inside the host-tools container (tools/hosttools.sh).
set -euo pipefail
if [[ ! -e /run/.containerenv ]]; then
  exec "$(dirname "$0")/hosttools.sh" "$0" "$@"
fi

usage="usage: extract-image.sh <sdk-system-image-dir | ci-img.zip> <out-dir>"
src=${1:?$usage}
out=${2:?$usage}
work="$out/.work"
rm -rf "$work"
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

# Unpack a boot image's ramdisk (lz4 legacy or gzip cpio) into $2.
ramdisk_extract() {
  local rd=$1 dest=$2
  rm -rf "$dest"
  mkdir -p "$dest"
  if [[ $(file -b "$rd") == *LZ4* ]] || lz4 -t "$rd" 2>/dev/null; then
    lz4 -dc "$rd"
  else
    gzip -dc "$rd"
  fi | (cd "$dest" && cpio -idm --quiet 2>/dev/null || true) # device nodes fail
}

if [[ -f $src && $src == *.zip ]]; then
  # AOSP CI image zip: a sparse super.img holding the dynamic partitions
  # (A/B: take the _a slot; the _b slot is empty or system_other).
  python3 -Ic "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2], ['super.img','init_boot.img'])" "$src" "$work"
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
  unpack_bootimg --boot_img "$work/init_boot.img" --out "$work/init_boot" >/dev/null
  ramdisk_extract "$work/init_boot/ramdisk" "$out/ramdisk"
else
  gpt_extract "$src/system.img" super "$work/super.img"
  lpunpack "$work/super.img" "$work/" >/dev/null
  gpt_extract "$src/vendor.img" vendor "$work/vendor.img"
  for p in "$work"/*.img; do
    name=$(basename "$p" .img)
    [[ $name == super ]] && continue
    [[ $(stat -c %s "$p") -eq 0 ]] && continue
    name=${name%_a}
    echo "extracting $name"
    fs_extract "$p" "$out/$name"
  done
  ramdisk_extract "$src/ramdisk.img" "$out/ramdisk"
  cp "$src/build.prop" "$out/" 2>/dev/null || true
fi
rm -rf "$work"
echo "done: $(ls "$out")"
