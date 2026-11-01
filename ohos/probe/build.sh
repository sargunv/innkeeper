#!/usr/bin/env bash
# build:ohos-probe: InnkeeperProbes.hap, the app that the ohos e2e group
# installs, into out/test/probes. It builds from this directory's sources with
# the SDK that ohos/sdk.sh fetches, and without hvigor:
#   restool      resources and module.json  -> resources.index, module.json
#   arkts.js     ets/                       -> ets/modules.abc
#   packing tool all of the above           -> the unsigned HAP
#   sign.sh      the unsigned HAP           -> InnkeeperProbes.hap
set -euo pipefail
top=$(cd "$(dirname "$0")/../.." && pwd)
cd "$top"
source ohos/sdk.env
sdk=$(ohos/sdk.sh)
src=$top/ohos/probe
work=$top/out/build/ohos-probe
bundle=dev.sargunv.innkeeper.probes
rm -rf "$work"
mkdir -p "$work/res" "$work/ets"
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
  node "$src/arkts.js" "$loader" "$sdk/rollup")
java -jar "$sdk/toolchains/lib/app_packing_tool.jar" --mode hap --force true \
  --json-path "$work/res/module.json" --resources-path "$work/res/resources" \
  --index-path "$work/res/resources.index" --ets-path "$work/ets" \
  --out-path "$work/unsigned.hap" >"$work/pack.log" 2>&1 || {
  cat "$work/pack.log" >&2
  exit 1
}
mkdir -p out/test/probes
ohos/probe/sign.sh "$sdk/toolchains/lib" "$bundle" "$work/unsigned.hap" out/test/probes/InnkeeperProbes.hap
