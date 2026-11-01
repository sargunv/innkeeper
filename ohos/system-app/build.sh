#!/usr/bin/env bash
# build:ohos-system-app: Innkeeper System, the system app that OpenHarmony
# preinstalls from our layer, into out/build/ohos-system-app/install:
#   share/innkeeper/ohos/sys_prod/app/InnkeeperSystem/InnkeeperSystem.hap
#   share/innkeeper/ohos/sys_prod/etc/app/install_list.json
#   share/innkeeper/ohos/sys_prod/etc/app/install_list_capability.json
# The native module, libinnkeeper.so, builds as the VDIs do (ohos/build.sh),
# against the musl headers that build:ohos copies out of the build container.
set -euo pipefail
top=$(cd "$(dirname "$0")/../.." && pwd)
cd "$top"
sdk=$(ohos/system-app/sdk.sh)
bundle=dev.sargunv.innkeeper.system
work=out/build/ohos-system-app
install=$work/install
out=$install/share/innkeeper/ohos/sys_prod
rm -rf "$work"
mkdir -p "$work/stubs" "$work/libs/x86_64" "$out/app/InnkeeperSystem" "$out/etc/app"

llvm=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
musl=out/build/ohos/musl
# Empty stand-ins give the linker each library's soname. The image's
# libraries resolve the symbols at load time. A system app's native module
# may link the system's libraries, such as libbegetutil for parameters.
for lib in libc.so libace_napi.z.so libbegetutil.z.so; do
  "$llvm/clang" --target=x86_64-linux-musl -shared -nostdlib -Wl,-soname,$lib \
    -o "$work/stubs/$lib" -x c /dev/null
done
so=$work/libs/x86_64/libinnkeeper.so
"$llvm/clang" --target=x86_64-linux-musl -O2 -fPIC -nostdinc -isystem "$musl/include" \
  -isystem "$("$llvm/clang" -print-resource-dir)/include" -isystem "$sdk/node-api/include" \
  -I guest/bridge-client/include -D_GNU_SOURCE -fvisibility=hidden -std=gnu17 \
  -Wall -Wextra -Werror -Wno-sign-compare \
  -shared -nostdlib -Wl,-z,now -Wl,-soname,libinnkeeper.so -o "$so" \
  ohos/system-app/native/napi.c ohos/system-app/native/connection.c \
  guest/bridge-client/bridge_client.c "$work"/stubs/*.so
"$llvm/llvm-strip" "$so"

ohos/hap.sh "$sdk" ohos/system-app "$work/hap" "$work/unsigned.hap" "$work/libs"
ohos/sign.sh "$sdk/toolchains/lib" "$bundle" --system "$work/unsigned.hap" \
  "$out/app/InnkeeperSystem/InnkeeperSystem.hap"

# The bundle manager installs each app that an install_list.json in a config
# layer names, and applies the capabilities that install_list_capability.json
# grants to the app with the matching signature. keepAlive starts the app's
# main element at boot, and a ServiceExtensionAbility needs
# allowAppUsePrivilegeExtension. hap-sign-tool issues a new
# app certificate at each signing, so the entry matches the appId, which is
# the bundle name and the signing key's public EC point.
app_id=$(keytool -exportcert -rfc -keystore "$sdk/toolchains/lib/OpenHarmony.p12" \
  -storepass 123456 -alias "openharmony application release" 2>/dev/null |
  node -e '
const crypto = require("crypto");
const pem = require("fs").readFileSync(0, "utf8");
const spki = new crypto.X509Certificate(pem).publicKey.export({ type: "spki", format: "der" });
process.stdout.write(spki.subarray(-65).toString("base64"));
')
cat >"$out/etc/app/install_list.json" <<EOF
{
  "install_list": [
    { "app_dir": "/sys_prod/app/InnkeeperSystem", "removable": false }
  ]
}
EOF
# The image's own entry for the background task service's resources lists a
# certificate fingerprint that its HAP doesn't have. Without singleton, the
# bundle manager installs that HAP for user 100 alone, and the service, which
# runs as user 0, can't load its notification texts. Every continuous task
# then fails with 9800006. The second entry matches the image's appId.
bgtask_res=com.ohos.backgroundtaskmgr.resources
bgtask_res_key=BBCORX5amL5E0TjFirHG8bR0FzBSi6V8E66NJAHISp4iPikRx0eCnlDERyiM06IfDTuREXh/RaYE/PWWcxVdzas=
cat >"$out/etc/app/install_list_capability.json" <<EOF
{
  "install_list": [
    {
      "bundleName": "$bundle",
      "app_signature": ["${bundle}_$app_id"],
      "keepAlive": true,
      "singleton": true,
      "allowAppUsePrivilegeExtension": true
    },
    {
      "bundleName": "$bgtask_res",
      "app_signature": ["${bgtask_res}_$bgtask_res_key"],
      "singleton": true,
      "allowAppDesktopIconHide": true
    }
  ]
}
EOF
