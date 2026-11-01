#!/usr/bin/env bash
# Makes the SDK that builds the system app in $OHOS_SDK_HOME/<version>-system,
# unless it is there already, and prints that directory. It is the SDK that
# ohos/sdk.sh fetches, with two additions that ohos/system-app/sdk.env pins:
#   ets/api    the SDK's declarations with the full ones from interface_sdk-js
#              over them, so that ArkTS code compiles against the system APIs
#   node-api/  Node's N-API headers, for the native module
# ets-loader reads the declarations relative to its own directory, so ets/ is
# a copy (a reflink where the filesystem allows it). toolchains/ and rollup/
# link to the base SDK.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
base=$("$here/../sdk.sh")
# shellcheck source=ohos/system-app/sdk.env
source "$here/sdk.env"
dir=$base-system
want=$(cat "$base/.stamp" "$here/sdk.env" "$0" | sha256sum | cut -d' ' -f1)
if [[ $(cat "$dir/.stamp" 2>/dev/null) == "$want" ]]; then
  echo "$dir"
  exit 0
fi
tmp=$(mktemp -d "$OHOS_SDK_HOME/.fetch.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
echo "system-app/sdk.sh: fetching the system API declarations" >&2
# A partial clone fetches only the files that make the declarations.
git init -q "$tmp/interface"
git -C "$tmp/interface" fetch -q --depth 1 --filter=blob:none "$OHOS_INTERFACE_URL" \
  "$OHOS_INTERFACE_COMMIT"
git -C "$tmp/interface" sparse-checkout set --no-cone /api/ /build-tools/handleApiFiles.js \
  /build-tools/handle_api_dynamic_delete.json /build-tools/intToNumber.js
git -C "$tmp/interface" -c advice.detachedHead=false checkout -q FETCH_HEAD
sum=$(cd "$tmp/interface" && find api build-tools -type f -print0 | LC_ALL=C sort -z |
  xargs -0 sha256sum | sha256sum | cut -d' ' -f1)
if [[ $sum != "$OHOS_INTERFACE_SHA256" ]]; then
  echo "system-app/sdk.sh: interface_sdk-js has checksum $sum, not $OHOS_INTERFACE_SHA256" >&2
  exit 1
fi
# The raw files declare both ArkTS 1.1 and 1.2. As in the SDK's build,
# handleApiFiles keeps 1.1's declarations, and intToNumber turns 1.2's integer
# types into number. Both need the TypeScript and commander that ets-loader
# bundles.
tools=$tmp/interface/build-tools
export NODE_PATH=$base/ets/build-tools/ets-loader/node_modules
if ! node "$tools/handleApiFiles.js" --path "$tmp/interface" --type ets \
  --output "$tmp/dynamic" --isPublic false >"$tmp/declarations.log" 2>&1 ||
  ! node "$tools/intToNumber.js" --path "$tmp/dynamic" --output "$tmp/declarations" \
    >>"$tmp/declarations.log" 2>&1 || grep -qi error "$tmp/declarations.log"; then
  cat "$tmp/declarations.log" >&2
  exit 1
fi
curl -fsSL -o "$tmp/node-api.tgz" "$NODE_API_HEADERS_URL"
echo "$NODE_API_HEADERS_SHA256  $tmp/node-api.tgz" | sha256sum -c --quiet
mkdir "$tmp/sdk" "$tmp/sdk/node-api"
tar -xzf "$tmp/node-api.tgz" -C "$tmp/sdk/node-api" --strip-components=1 \
  package/include package/LICENSE
cp -r --reflink=auto "$base/ets" "$tmp/sdk/ets"
cp -r "$tmp/declarations/api/." "$tmp/sdk/ets/api/"
ln -s "$base/toolchains" "$base/rollup" "$tmp/sdk/"
rm -rf "$dir"
echo "$want" >"$tmp/sdk/.stamp"
mv "$tmp/sdk" "$dir"
echo "$dir"
