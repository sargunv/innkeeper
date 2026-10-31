#!/usr/bin/env bash
# Download a guest's pinned image, verify it, and extract its partitions with
# the guest's hook, extract-<system>.sh (see image/README.md):
#   <images>/<name>/dl/   the zip from the pin (resumable), deleted once
#                         extracted
#   <images>/<name>/img/  the partitions
# For Android, the session flattens the APEXes itself (per vendor APEX
# selection).
#
# usage: fetch.sh <android|ohos> <pin.toml> <images>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
usage="usage: fetch.sh <android|ohos> <pin.toml> <images>"
system=${1:?$usage} pin=${2:?$usage} images=${3:?$usage}

die() {
  echo "fetch.sh: $*" >&2
  exit 1
}

[[ $system == android || $system == ohos ]] || die "no such system: $system"
IFS=$'\t' read -r name url size sha < <(
  python3 -Ic 'import sys, tomllib; p = tomllib.load(open(sys.argv[1], "rb")); print(*(p[k] for k in ("name", "url", "size", "sha256")), sep="\t")' "$pin"
)
[[ $name =~ ^[a-z0-9.-]+$ && $name != .* ]] || die "bad image name in $pin: $name"
dest=$images/$name
mkdir -p "$dest/dl"

# One fetch per image at a time (several profiles may start at once).
exec 9>"$dest/.lock"
flock 9

# Extracted images may have read-only directories.
remove() { if [[ -e $1 ]]; then chmod -R u+w "$1" && rm -rf "$1"; fi; }
zip=$dest/dl/${url##*/}
extracted() { [[ $(cat "$dest/img/.source" 2>/dev/null) == "$sha" ]]; }

verify() { # file size sha256
  [[ -f $1 && $(stat -c %s "$1") == "$2" ]] && [[ $(sha256sum "$1" | cut -d' ' -f1) == "$3" ]]
}

if extracted; then
  :
elif verify "$zip" "$size" "$sha"; then
  echo "have ${zip##*/}"
else
  rm -f "$zip"
  echo "downloading ${zip##*/} ($((size / 1048576)) MiB) from $url"
  # Android's CI serves GET only (HEAD is 404); -C - resumes a partial
  # download.
  if [[ ! -f $zip.part || $(stat -c %s "$zip.part") -lt $size ]]; then
    curl -fL --retry 5 --retry-delay 5 -C - -o "$zip.part" "$url"
  fi
  verify "$zip.part" "$size" "$sha" || {
    rm -f "$zip.part"
    die "${zip##*/}: size or SHA-256 mismatch (expected $size bytes, sha256 $sha); removed it, try again"
  }
  mv "$zip.part" "$zip"
fi

if extracted; then
  echo "already extracted: $dest/img"
else
  remove "$dest/img.tmp"
  "$here/extract-$system.sh" "$zip" "$dest/img.tmp"
  echo "$sha" >"$dest/img.tmp/.source"
  remove "$dest/img"
  mv "$dest/img.tmp" "$dest/img"
  echo "extracted $name into $dest/img"
fi
rm -f "$zip"
