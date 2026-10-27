#!/usr/bin/env bash
# Download the pinned Android image from Android's public CI, verify it, and
# extract its partitions (see docs/image.md):
#   $LINUXAPK_IMAGES/<name>/dl/   artifacts from image/pin.toml (resumable)
#   $LINUXAPK_IMAGES/<name>/img/  system, system_ext, product, vendor, odm,
#                                 *_dlkm (image/extract.sh)
# LINUXAPK_IMAGES defaults to ~/.local/share/linuxapk/images. The session
# flattens the APEXes itself (per vendor APEX selection), on first start.
#
# usage: fetch-image.sh [pin.toml]     (default: image/pin.toml)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
pin=${1:-$repo/image/pin.toml}
images=${LINUXAPK_IMAGES:-$HOME/.local/share/linuxapk/images}

die() {
  echo "fetch-image: $*" >&2
  exit 1
}

# name, then one "url<TAB>file<TAB>size<TAB>sha256" line per artifact.
parsed=$(
  python3 -I - "$pin" <<'EOF'
import sys, tomllib
p = tomllib.load(open(sys.argv[1], 'rb'))
print(p['name'])
for a in p['artifact']:
    url = p['url'].format(build_id=p['build_id'], target=p['target'], artifact=a['name'])
    print(f"{url}\t{a['name'].rsplit('/', 1)[-1]}\t{a['size']}\t{a['sha256']}")
EOF
)
name=$(head -1 <<<"$parsed")
[[ $name =~ ^[a-z0-9-]+$ ]] || die "bad image name in $pin: $name"
dest=$images/$name
mkdir -p "$dest/dl"

# One fetch per image at a time (sessions of several lanes may start at once).
exec 9>"$dest/.lock"
flock 9

verify() { # file size sha256
  [[ -f $1 && $(stat -c %s "$1") == "$2" ]] && [[ $(sha256sum "$1" | cut -d' ' -f1) == "$3" ]]
}

while IFS=$'\t' read -r url file size sha; do
  f=$dest/dl/$file
  if verify "$f" "$size" "$sha"; then
    echo "have $file"
    continue
  fi
  rm -f "$f"
  echo "downloading $file ($((size / 1048576)) MiB) from $url"
  # The CI serves GET only (HEAD is 404); -C - resumes a partial download.
  if [[ ! -f $f.part || $(stat -c %s "$f.part") -lt $size ]]; then
    curl -fL --retry 5 --retry-delay 5 -C - -o "$f.part" "$url"
  fi
  verify "$f.part" "$size" "$sha" || {
    rm -f "$f.part"
    die "$file: size or SHA-256 mismatch (expected $size bytes, sha256 $sha); removed it, try again"
  }
  mv "$f.part" "$f"
done < <(tail -n +2 <<<"$parsed")

# Extract once per verified image zip.
zip=$(tail -n +2 <<<"$parsed" | cut -f2 | grep -- '-img-.*\.zip$' | head -1)
zsha=$(tail -n +2 <<<"$parsed" | awk -F'\t' -v z="$zip" '$2 == z {print $4}')
if [[ $(cat "$dest/img/.source" 2>/dev/null) == "$zsha" ]]; then
  echo "already extracted: $dest/img"
else
  rm -rf "$dest/img.tmp"
  "$here/extract.sh" "$dest/dl/$zip" "$dest/img.tmp"
  echo "$zsha" >"$dest/img.tmp/.source"
  rm -rf "$dest/img"
  mv "$dest/img.tmp" "$dest/img"
  echo "extracted $name into $dest/img"
fi
