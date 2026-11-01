#!/usr/bin/env bash
# Extract an erofs or ext4 filesystem image into a directory, unprivileged,
# with the static tools from image/tools (fsck.erofs, debugfs).
#
# usage: fs-extract.sh <fs-image> <out-dir>
set -euo pipefail
tools=$(cd "$(dirname "$0")/.." && pwd)
usage="usage: fs-extract.sh <fs-image> <out-dir>"
fs=${1:?$usage} out=${2:?$usage}

# The superblock magic is at byte 1024 for erofs and at byte 1080 for ext4.
magic() { od -An -tx1 -j "$1" -N "$2" "$fs" | tr -d ' \n'; }
mkdir -p "$out"
if [[ $(magic 1024 4) == e2e1f5e0 ]]; then
  "$tools/fsck.erofs" --extract="$out" --no-preserve "$fs" >/dev/null
elif [[ $(magic 1080 2) == 53ef ]]; then
  # rdump puts the image root's children directly in <out-dir>.
  "$tools/debugfs" -R "rdump / $out" "$fs" >/dev/null 2>&1
else
  echo "fs-extract.sh: $fs is neither erofs nor ext4" >&2
  exit 1
fi
rm -rf "$out/lost+found"
