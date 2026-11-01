#!/usr/bin/env bash
# fetch.sh's hook for OpenHarmony. It extracts Oniro's emulator zip into plain
# directory trees, read-only as on a mount:
#   <out>/system  system.img's system-as-root tree
#   <out>/vendor
# It unpacks one raw ext4 image at a time, because system.img alone is 3 GiB,
# and extracts each with fs-extract.sh's static debugfs. The guest runs on the
# host kernel, so the guest kernel, ramdisk and other images are left out.
#
# usage: extract-ohos.sh <oniro_emulator.zip> <out-dir>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
usage="usage: extract-ohos.sh <oniro_emulator.zip> <out-dir>"
zip=${1:?$usage} out=${2:?$usage}
mkdir -p "$out"
for part in system vendor; do
  python3 -Ic "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extract(sys.argv[2], sys.argv[3])" \
    "$zip" "images/$part.img" "$out"
  "$here/fs-extract.sh" "$out/images/$part.img" "$out/$part"
  rm "$out/images/$part.img"
done
rmdir "$out/images"
if [[ ! -x $out/system/system/bin/init || ! -d $out/vendor/etc ]]; then
  echo "extract-ohos.sh: unexpected partition layout in ${zip##*/}" >&2
  exit 1
fi
# The guest runs as this user and must not change the image it boots.
chmod -R a-w "$out/system" "$out/vendor"
