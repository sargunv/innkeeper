#!/usr/bin/env bash
# Flatten the packed APEXes of an extracted image into plain directories, the
# way apexd would mount them:
#   <out>/<apex-name>/...          payload contents (name from apex_manifest.pb)
#   <out>/apex-info-list.xml       what apexd writes to /apex/apex-info-list.xml
# Unprivileged: runs inside the host-tools container (tools/hosttools.sh).
#
# usage: flatten-apex.sh <extracted-image-dir> <out-dir>
#
# Multi-install vendor APEXes (several files with one manifest name, e.g.
# cuttlefish's keymint.rust_nonsecure / keymint.rust_cf_remote) are selected
# the way apexd does it: androidboot.vendor.apex.<name>=<file without .apex>
# (ro.boot.vendor.apex.*) picks one, and "none" drops a vendor APEX. The
# selections are read from image/cmdline (whitespace-separated
# androidboot.*=value tokens, # comments), which the session also passes to
# init.
set -euo pipefail
if [[ ! -e /run/.containerenv ]]; then
  exec "$(dirname "$0")/hosttools.sh" "$0" "$@"
fi

img=$(realpath "${1:?usage: flatten-apex.sh <img-dir> <out-dir>}")
out=${2:?usage: flatten-apex.sh <img-dir> <out-dir>}
cmdline=$(dirname "$0")/../image/cmdline
declare -A sel=()
while read -r k v; do sel[$k]=$v; done < <(sed 's/#.*//' "$cmdline" | tr -s ' \t' '\n' |
  sed -n 's/^androidboot\.vendor\.apex\.\([^=]*\)=\(.*\)/\1 \2/p')
mkdir -p "$out"
out=$(realpath "$out")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Print "name<TAB>version" from an apex_manifest.pb (proto3: 1=name, 2=version).
manifest_info() {
  python3 -I - "$1" <<'EOF'
import sys
b = open(sys.argv[1], 'rb').read()
i, name, ver = 0, '', 0
def varint():
    global i
    r = s = 0
    while True:
        c = b[i]; i += 1
        r |= (c & 0x7f) << s; s += 7
        if c < 0x80: return r
while i < len(b):
    key = varint(); f, t = key >> 3, key & 7
    if t == 0:
        v = varint()
        if f == 2: ver = v
    elif t == 2:
        n = varint(); v = b[i:i+n]; i += n
        if f == 1: name = v.decode()
    elif t == 1: i += 8
    elif t == 5: i += 4
    else: raise SystemExit(f'bad wire type {t}')
print(f'{name}\t{ver}')
EOF
}

extract_fs() {
  local fsimg=$1 dest=$2
  if [[ $(file -b "$fsimg") == *ext[24]* ]]; then
    debugfs -R "rdump / $dest" "$fsimg" >/dev/null 2>&1
    # rdump puts the image root's children directly in $dest.
  else
    fsck.erofs --extract="$dest" --no-preserve "$fsimg" >/dev/null
  fi
}

declare -A partition_of=(
  ["system/system/apex"]=SYSTEM ["system_ext/apex"]=SYSTEM_EXT
  ["product/apex"]=PRODUCT ["vendor/apex"]=VENDOR ["odm/apex"]=ODM)
declare -A mountpoint_of=(
  ["system/system/apex"]=/system/apex ["system_ext/apex"]=/system_ext/apex
  ["product/apex"]=/product/apex ["vendor/apex"]=/vendor/apex ["odm/apex"]=/odm/apex)

declare -A seen=()
list="$work/apex-info-list.xml"
echo '<?xml version="1.0" encoding="utf-8"?>' >"$list"
echo '<apex-info-list>' >>"$list"

for rel in system/system/apex system_ext/apex product/apex vendor/apex odm/apex; do
  [[ -d $img/$rel ]] || continue
  for apex in "$img/$rel"/*.apex "$img/$rel"/*.capex; do
    [[ -e $apex ]] || continue
    file=$(basename "$apex")
    if [[ $file == *.capex ]]; then
      echo "skipping compressed $file (unsupported)" >&2
      continue
    fi
    rm -rf "$work/x"
    mkdir -p "$work/x"
    (cd "$work/x" && python3 -Ic "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall('.', ['apex_payload.img','apex_manifest.pb'])" "$apex")
    IFS=$'\t' read -r name version < <(manifest_info "$work/x/apex_manifest.pb")
    if [[ -n ${sel[$name]:-} && ($rel == vendor/apex || $rel == odm/apex) ]]; then
      if [[ ${sel[$name]} != "${file%.apex}" ]]; then
        echo "skipping $file (androidboot.vendor.apex.$name=${sel[$name]})"
        continue
      fi
    fi
    if [[ -n ${seen[$name]:-} ]]; then
      echo "two APEXes for $name: ${seen[$name]} and $file; select one with androidboot.vendor.apex.$name" >&2
      exit 1
    fi
    seen[$name]=$file
    dest="$out/$name"
    rm -rf "$dest"
    mkdir -p "$dest"
    extract_fs "$work/x/apex_payload.img" "$dest"
    rm -rf "$dest/lost+found"
    # apexd exposes the manifest at /apex/<name>/apex_manifest.pb; the payload
    # already carries it, but make sure.
    [[ -e $dest/apex_manifest.pb ]] || cp "$work/x/apex_manifest.pb" "$dest/"
    path="${mountpoint_of[$rel]}/$file"
    printf '    <apex-info moduleName="%s" modulePath="%s" preinstalledModulePath="%s" versionCode="%s" versionName="" isFactory="true" isActive="true" lastUpdateMillis="0" provideSharedApexLibs="false" partition="%s"/>\n' \
      "$name" "$path" "$path" "$version" "${partition_of[$rel]}" >>"$list"
    echo "$name ($file, v$version)"
  done
done
echo '</apex-info-list>' >>"$list"
cp "$list" "$out/apex-info-list.xml"
echo "flattened $(grep -c '<apex-info ' "$list") APEXes into $out"
