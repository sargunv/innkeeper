#!/usr/bin/env bash
# Fetch microG (GmsCore, GsfProxy, FakeStore) from microG's own F-Droid repo,
# verify it, and install it as privileged system apps into the microg addon,
# a layer the session stacks over the partitions
# ($XDG_DATA_HOME/linuxapk/addons/microg/system_ext; see android/microg/README.md):
#   priv-app/GmsCore/GmsCore.apk (+ lib/x86_64/, extracted)
#   priv-app/GsfProxy/GsfProxy.apk
#   priv-app/FakeStore/FakeStore.apk
#   etc/permissions/privapp-permissions-microg.xml
#   etc/default-permissions/default-permissions-microg.xml
#   etc/sysconfig/microg.xml
# Downloads go in $XDG_CACHE_HOME/linuxapk/microg. Restart the session
# afterwards.
#
#   android/microg/install.sh            # latest stable release
#   android/microg/install.sh --beta     # latest release, including the Beta channel
#   android/microg/install.sh --remove   # remove the addon again
#
# Trust chain: the repo's entry.jar must be signed by microG's published key
# (SHA-256 9BD06727..., github.com/microg/GmsCore/wiki/Downloads), entry.json
# pins index-v2.json by SHA-256, the index pins each APK by SHA-256, and
# apksigner checks that each APK is signed by the same microG key.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
bt=$ANDROID_HOME/build-tools/$ANDROID_BUILD_TOOLS
url=https://microg.org/fdroid/repo
key=9bd06727e62796c0130eb6dab39b73157451582cbd138e86c468acc395d14165
dl=${XDG_CACHE_HOME:-$HOME/.cache}/linuxapk/microg
addon=${XDG_DATA_HOME:-$HOME/.local/share}/linuxapk/addons/microg
dst=$addon/system_ext
# package  priv-app name
apps=(
  com.google.android.gms GmsCore
  com.google.android.gsf GsfProxy
  com.android.vending FakeStore
)
xmls=(
  permissions/privapp-permissions-microg.xml
  default-permissions/default-permissions-microg.xml
  sysconfig/microg.xml
)

die() {
  echo "microg: $*" >&2
  exit 1
}

if [[ ${1:-} == --remove ]]; then
  rm -rf "$addon"
  echo "removed $addon"
  exit 0
fi
beta=0
[[ ${1:-} == --beta ]] && beta=1

mkdir -p "$dl"
cd "$dl"
curl -sSfL -o entry.jar "$url/entry.jar"
# jarsigner must report entry.json as signed and verified ("sm"); keytool
# shows the signer. The key is self-signed, so -strict would always fail.
jv=$(jarsigner -verify -verbose entry.jar 2>&1) || die "entry.jar: bad signature"
if ! grep -q '^jar verified\.$' <<<"$jv" || ! grep -Eq '^sm +[0-9]+ .* entry\.json$' <<<"$jv"; then
  die "entry.jar: entry.json is not signed"
fi
signer=$(keytool -printcert -jarfile entry.jar | sed -n 's/^\s*SHA256: //p' | tr -d ': ' | tr 'A-F' 'a-f')
[[ $signer == "$key" ]] || die "entry.jar signed by $signer, not microG"
unzip -p entry.jar entry.json >entry.json.tmp
mv entry.json.tmp entry.json

# index-v2.json, checked against entry.json; then one
# "package<TAB>file<TAB>sha256<TAB>version" line per app.
idx=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["index"]["sha256"])' entry.json)
curl -sSfL -o index-v2.json "$url/index-v2.json"
echo "$idx  index-v2.json" | sha256sum -c --status || die "index-v2.json: checksum mismatch"
picks=$(
  python3 -I - index-v2.json "$beta" "${apps[@]}" <<'EOF'
import json, sys
idx = json.load(open(sys.argv[1]))
beta = sys.argv[2] == '1'
for pkg in sys.argv[3::2]:
    vers = [v for v in idx['packages'][pkg]['versions'].values()
            if beta or not v.get('releaseChannels')]
    v = max(vers, key=lambda v: v['manifest']['versionCode'])
    f = v['file']
    print(pkg, f['name'].lstrip('/'), f['sha256'], v['manifest']['versionName'], sep='\t')
EOF
)

mkdir -p "$dst/etc"
for ((i = 0; i < ${#apps[@]}; i += 2)); do
  pkg=${apps[i]} name=${apps[i + 1]}
  IFS=$'\t' read -r _ file sum ver < <(grep "^$pkg"$'\t' <<<"$picks")
  [[ $file =~ ^[A-Za-z0-9._-]+\.apk$ ]] || die "bad file name: $file"
  if ! echo "$sum  $file" | sha256sum -c --status 2>/dev/null; then
    curl -sSfL -o "$file.part" "$url/$file"
    mv "$file.part" "$file"
    echo "$sum  $file" | sha256sum -c --status || die "$file: checksum mismatch"
  fi
  cert=$("$bt/apksigner" verify --print-certs "$file" 2>/dev/null |
    sed -n 's/^Signer #1 certificate SHA-256 digest: //p;s/^V[0-9] Signer: certificate SHA-256 digest: //p' | sort -u)
  [[ $cert == "$key" ]] || die "$file signed by $cert, not microG"

  app=$dst/priv-app/$name
  rm -rf "$app"
  mkdir -p "$app"
  cp "$file" "$app/$name.apk"
  # System apps don't get their native libraries extracted (PackageAbiHelperImpl
  # shouldExtractLibs), and GmsCore's are compressed: ship them unpacked.
  if unzip -l "$file" 'lib/x86_64/*' >/dev/null 2>&1; then
    mkdir -p "$app/lib/x86_64"
    unzip -qjo "$file" 'lib/x86_64/*' -d "$app/lib/x86_64"
  fi
  echo "$name: $pkg $ver ($file)"
done
for x in "${xmls[@]}"; do
  mkdir -p "$dst/etc/$(dirname "$x")"
  cp "$here/etc/$x" "$dst/etc/$x"
done
chmod -R go+rX "$addon"
echo "installed microG into $addon; restart the session"
