#!/usr/bin/env bash
# Download the pinned Android image from Android's public CI, verify it, and
# extract its partitions (see image/README.md):
#   <images>/<name>/dl/   the image zip from image/pin.toml (resumable),
#                         deleted once extracted
#   <images>/<name>/img/  system, system_ext, product, vendor, odm, *_dlkm
#                         (image/extract.sh)
# The session flattens the APEXes itself (per vendor APEX selection).
#
# usage: fetch.sh <pin.toml> <images>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
usage="usage: fetch.sh <pin.toml> <images>"
pin=${1:?$usage} images=${2:?$usage}

die() {
  echo "fetch.sh: $*" >&2
  exit 1
}

IFS=$'\t' read -r name url size sha < <(
  python3 -Ic 'import sys, tomllib; p = tomllib.load(open(sys.argv[1], "rb")); print(*(p[k] for k in ("name", "url", "size", "sha256")), sep="\t")' "$pin"
)
[[ $name =~ ^[a-z0-9-]+$ ]] || die "bad image name in $pin: $name"
dest=$images/$name
mkdir -p "$dest/dl"

# One fetch per image at a time (several profiles may start at once).
exec 9>"$dest/.lock"
flock 9

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
  # The CI serves GET only (HEAD is 404); -C - resumes a partial download.
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
  rm -rf "$dest/img.tmp"
  "$here/extract.sh" "$zip" "$dest/img.tmp"
  echo "$sha" >"$dest/img.tmp/.source"
  rm -rf "$dest/img"
  mv "$dest/img.tmp" "$dest/img"
  echo "extracted $name into $dest/img"
fi
rm -f "$zip"
