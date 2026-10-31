#!/usr/bin/env bash
# Download the pinned OpenHarmony image, verify it, and extract system.img
# and vendor.img into <images>/<name>/img/ (ohos/README.md#image).
#
# usage: fetch.sh <pin.toml> <images> <fs-extract.sh>
set -euo pipefail
usage="usage: fetch.sh <pin.toml> <images> <fs-extract.sh>"
pin=${1:?$usage} images=${2:?$usage} fsx=${3:?$usage}

die() {
  echo "fetch.sh: $*" >&2
  exit 1
}
# The image has read-only directories.
remove() { if [[ -e $1 ]]; then chmod -R u+w "$1" && rm -rf "$1"; fi; }

# name, url, size, sha256: one per line.
mapfile -t p < <(
  python3 -I - "$pin" <<'PY'
import sys, tomllib
p = tomllib.load(open(sys.argv[1], 'rb'))
for k in ('name', 'url', 'size', 'sha256'):
    print(p[k])
PY
)
name=${p[0]} url=${p[1]} size=${p[2]} sha=${p[3]}
[[ $name =~ ^[a-z0-9.-]+$ ]] || die "bad image name in $pin: $name"
dest=$images/$name
mkdir -p "$dest/dl"
exec 9>"$dest/.lock"
flock 9

if [[ $(cat "$dest/img/.source" 2>/dev/null) == "$sha" ]]; then
  echo "already extracted: $dest/img"
  exit 0
fi
zip=$dest/dl/${url##*/}
verify() { [[ -f $1 && $(stat -c %s "$1") == "$size" && $(sha256sum "$1" | cut -d' ' -f1) == "$sha" ]]; }
if verify "$zip"; then
  echo "have ${zip##*/}"
else
  rm -f "$zip"
  echo "downloading ${zip##*/} ($((size / 1048576)) MiB) from $url"
  curl -fL --retry 5 --retry-delay 5 -C - -o "$zip.part" "$url"
  verify "$zip.part" || {
    rm -f "$zip.part"
    die "${zip##*/}: size or SHA-256 mismatch (expected $size bytes, sha256 $sha); removed it, try again"
  }
  mv "$zip.part" "$zip"
fi

work=$dest/img.tmp
remove "$work"
mkdir -p "$work"
for part in system vendor; do
  # One raw ext4 image at a time: system.img alone is 3 GiB.
  python3 -Ic "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extract(sys.argv[2], sys.argv[3])" \
    "$zip" "images/$part.img" "$work"
  "$fsx" "$work/images/$part.img" "$work/$part"
  rm "$work/images/$part.img"
done
rmdir "$work/images"
[[ -x $work/system/system/bin/init && -d $work/vendor/etc ]] || die "unexpected partition layout in ${zip##*/}"
# As on read-only mounts: the guest runs as this user, and must not change
# the image it boots.
chmod -R a-w "$work/system" "$work/vendor"
echo "$sha" >"$work/.source"
remove "$dest/img"
mv "$work" "$dest/img"
rm -f "$zip"
echo "extracted $name into $dest/img"
