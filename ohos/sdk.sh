#!/usr/bin/env bash
# Fetches the OpenHarmony SDK parts and the rollup that ohos/sdk.env pins into
# $OHOS_SDK_HOME/<version>, unless they are there already, and prints that
# directory. The SDK parts are the ArkTS compiler (ets/), restool, the packing
# tool, hap-sign-tool, and the public signing keys and profile templates
# (toolchains/). Each is Apache 2.0 open source, and none needs an account.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ohos/sdk.env
source "$here/sdk.env"
: "${OHOS_SDK_HOME:?set by mise.toml}"
dir=$OHOS_SDK_HOME/$OHOS_SDK_VERSION
want=$(cat "$here/sdk.env" "$0" | sha256sum | cut -d' ' -f1)
if [[ $(cat "$dir/.stamp" 2>/dev/null) == "$want" ]]; then
  echo "$dir"
  exit 0
fi
mkdir -p "$OHOS_SDK_HOME"
tmp=$(mktemp -d "$OHOS_SDK_HOME/.fetch.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
echo "sdk.sh: fetching the OpenHarmony SDK $OHOS_SDK_VERSION" >&2
zips=(toolchains ets)
names=()
for z in "${zips[@]}"; do names+=("ohos-sdk/linux/$z-linux-x64-$OHOS_SDK_VERSION-Release.zip"); done
# tar exits after the last named member, and curl then fails on the closed
# pipe. The checksums catch a short or failed download.
curl -fsSL "$OHOS_SDK_URL" 2>"$tmp/curl.log" |
  tar -xzf - -C "$tmp" --occurrence=1 "${names[@]}" 2>>"$tmp/curl.log" || true
sha256sum -c --quiet <<EOF || {
$OHOS_SDK_TOOLCHAINS_SHA256  $tmp/${names[0]}
$OHOS_SDK_ETS_SHA256  $tmp/${names[1]}
EOF
  cat "$tmp/curl.log" >&2
  exit 1
}
curl -fsSL -o "$tmp/rollup.tgz" "$ROLLUP_URL"
echo "$ROLLUP_SHA256  $tmp/rollup.tgz" | sha256sum -c --quiet
mkdir "$tmp/sdk" "$tmp/sdk/rollup"
tar -xzf "$tmp/rollup.tgz" -C "$tmp/sdk/rollup" --strip-components=1
# Extract the entries under the kept prefixes, except the dropped ones, and
# keep their modes. The dropped files are the AOT compiler, a profiler and
# the API 8 compiler, which the build doesn't run.
python3 -I - "$tmp/sdk" "$tmp/${names[0]}" "$tmp/${names[1]}" <<'EOF'
import os, sys, zipfile
keep = ("toolchains/restool", "toolchains/lib/app_packing_tool.jar",
        "toolchains/lib/hap-sign-tool.jar", "toolchains/lib/OpenHarmony",
        "toolchains/lib/Unsgned", "toolchains/NOTICE.txt",
        "ets/api/", "ets/arkts/", "ets/component/", "ets/kits/",
        "ets/build-tools/ets-loader/", "ets/oh-uni-package.json", "ets/NOTICE.txt")
drop = ("ets/build-tools/ets-loader/bin/ark/build/legacy_api8/",
        "ets/build-tools/ets-loader/bin/ark/build/bin/ark_aot_compiler",
        "ets/build-tools/ets-loader/bin/ark/build/bin/profdump")
out = sys.argv[1]
for path in sys.argv[2:]:
    with zipfile.ZipFile(path) as z:
        for info in z.infolist():
            name = info.filename
            if not name.startswith(keep) or name.startswith(drop) or name.endswith("/"):
                continue
            target = z.extract(info, out)
            mode = info.external_attr >> 16 & 0o777
            if mode:
                os.chmod(target, mode)
EOF
rm -rf "$dir"
echo "$want" >"$tmp/sdk/.stamp"
mv "$tmp/sdk" "$dir"
echo "$dir"
