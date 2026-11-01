#!/usr/bin/env bash
# Builds an unsigned HAP from an app's sources with the SDK that ohos/sdk.sh
# fetches, and without hvigor:
#   restool      resources and module.json  -> resources.index, module.json
#   arkts.js     ets/                       -> ets/modules.abc
#   packing tool all of the above and libs  -> the HAP
# `libs` is a directory of native libraries by ABI, such as x86_64/libfoo.so.
#
# usage: hap.sh <sdk dir> <app source dir> <work dir> <out.hap> [libs]
set -euo pipefail
usage="usage: hap.sh <sdk dir> <app source dir> <work dir> <out.hap> [libs]"
sdk=${1:?$usage} src=${2:?$usage} work=${3:?$usage} out=${4:?$usage} libs=${5:-}
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ohos/sdk.env
source "$here/sdk.env"
src=$(cd "$src" && pwd)
bundle=$(node -p 'JSON.parse(require("fs").readFileSync(process.argv[1])).app.bundleName' "$src/module.json")
rm -rf "$work"
mkdir -p "$work/res" "$work/ets"
work=$(cd "$work" && pwd)
"$sdk/toolchains/restool" -i "$src" -j "$src/module.json" -p "$bundle" -o "$work/res" \
  -r "$work/ResourceTable.txt" -f >"$work/restool.log" || {
  cat "$work/restool.log" >&2
  exit 1
}
cat >"$work/loader.json" <<EOF
{"modulePathMap": {"entry": "$src"}, "projectRootPath": "$src", "moduleName": "entry",
 "compileMode": "esmodule", "packageManagerType": "ohpm", "nodeModulesPath": "$work/node_modules",
 "harNameOhmMap": {}, "hspNameOhmMap": {}, "byteCodeHarInfo": {}, "dynamicImportLibInfo": {},
 "compileEntry": [], "declarationEntry": [], "routerMap": []}
EOF
loader=$sdk/ets/build-tools/ets-loader
(cd "$loader" && env compileMode=esmodule buildMode=debug apiVersion="$OHOS_SDK_API" \
  aceModuleJsonPath="$work/res/module.json" aceModuleRoot="$src/ets" aceModuleBuild="$work/ets" \
  aceProfilePath="$work/res/resources/base/profile" aceBuildJson="$work/loader.json" \
  appResource="$work/ResourceTable.txt" cachePath="$work/cache" \
  node "$here/arkts.js" "$loader" "$sdk/rollup")
pack=()
if [[ -n $libs ]]; then pack=(--lib-path "$libs"); fi
java -jar "$sdk/toolchains/lib/app_packing_tool.jar" --mode hap --force true \
  --json-path "$work/res/module.json" --resources-path "$work/res/resources" \
  --index-path "$work/res/resources.index" --ets-path "$work/ets" "${pack[@]}" \
  --out-path "$out" >"$work/pack.log" 2>&1 || {
  cat "$work/pack.log" >&2
  exit 1
}
