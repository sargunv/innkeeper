#!/usr/bin/env bash
# Signs a HAP with OpenHarmony's public keys. The keys, certificates and
# profile templates come from the SDK's toolchains/lib, and the key passwords
# are the SDK's published ones.
#
# With a UDID, the profile is a debug profile for that device, as a developer
# signs an app under test. The bundle manager installs it only on the device
# with that UDID, which `innkeeper profile list` shows.
#
# With --system, the profile is a release profile for a system app at the
# system_basic level. It names no device, so the HAP installs on every
# profile, and the bundle manager accepts its level when the HAP is
# preinstalled from our layer.
#
# usage: sign.sh <toolchains/lib dir> <bundle name> <udid | --system> <in.hap> <out.hap>
set -euo pipefail
usage="usage: sign.sh <toolchains/lib dir> <bundle name> <udid | --system> <in.hap> <out.hap>"
lib=${1:?$usage} bundle=${2:?$usage} device=${3:?$usage} in=${4:?$usage} out=${5:?$usage}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
ks=$lib/OpenHarmony.p12
pw=(-keystoreFile "$ks" -keystorePwd 123456 -keyPwd 123456)
sign() { java -jar "$lib/hap-sign-tool.jar" "$@" >"$work/log" 2>&1 || {
  cat "$work/log" >&2
  return 1
}; }
for ca in "application ca" "application root ca"; do
  keytool -exportcert -rfc -keystore "$ks" -storepass 123456 -alias "openharmony $ca" \
    >"$work/${ca// /-}.cer" 2>/dev/null
done
# The app certificate chains the SDK's application key through its application CA
# to its root, which devices trust.
sign generate-app-cert -keyAlias "openharmony application release" -signAlg SHA256withECDSA \
  -issuer "C=CN,O=OpenHarmony,OU=OpenHarmony Team,CN=OpenHarmony Application CA" \
  -issuerKeyAlias "openharmony application ca" -issuerKeyPwd 123456 \
  -subject "C=CN,O=OpenHarmony,OU=OpenHarmony Team,CN=OpenHarmony Application Release" \
  -subCaCertFile "$work/application-ca.cer" -rootCaCertFile "$work/application-root-ca.cer" \
  -outForm certChain -outFile "$work/app.pem" -validity 3650 "${pw[@]}"
if [[ $device == --system ]]; then
  kind=release template=UnsgnedReleasedProfileTemplate.json
else
  kind=debug template=UnsgnedDebugProfileTemplate.json
fi
# The profile names the bundle and the app certificate. It is valid for ten
# years from yesterday.
node -e '
const fs = require("fs");
const [template, pem, bundle, device, out] = process.argv.slice(1);
const p = JSON.parse(fs.readFileSync(template));
const leaf = fs.readFileSync(pem, "utf8").match(/-----BEGIN CERTIFICATE-----[^-]+-----END CERTIFICATE-----\n/)[0];
const now = Math.floor(Date.now() / 1000);
p.validity = { "not-before": now - 86400, "not-after": now + 3650 * 86400 };
const info = p["bundle-info"];
info["bundle-name"] = bundle;
if (device === "--system") {
  info["distribution-certificate"] = leaf;
  info.apl = "system_basic";
  info["app-feature"] = "hos_system_app";
  // The system app reads and removes the notifications of every app, and it
  // starts the abilities that they open, which may be private. Those
  // permissions are above the APL of the app, so the profile allows them by
  // ACL.
  p.acls = {
    "allowed-acls": ["ohos.permission.NOTIFICATION_CONTROLLER", "ohos.permission.START_INVISIBLE_ABILITY"],
  };
} else {
  info["development-certificate"] = leaf;
  p["debug-info"]["device-ids"] = [device];
}
fs.writeFileSync(out, JSON.stringify(p, null, 2));
' "$lib/$template" "$work/app.pem" "$bundle" "$device" "$work/profile.json"
sign sign-profile -keyAlias "openharmony application profile $kind" -signAlg SHA256withECDSA \
  -mode localSign -profileCertFile "$lib/OpenHarmonyProfile${kind^}.pem" \
  -inFile "$work/profile.json" -outFile "$work/profile.p7b" "${pw[@]}"
sign sign-app -keyAlias "openharmony application release" -signAlg SHA256withECDSA -mode localSign \
  -appCertFile "$work/app.pem" -profileFile "$work/profile.p7b" -inFile "$in" -outFile "$out" \
  -signCode 1 "${pw[@]}"
