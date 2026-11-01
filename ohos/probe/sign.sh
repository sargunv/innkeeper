#!/usr/bin/env bash
# Signs a HAP with OpenHarmony's public keys and a debug profile for one
# device, as a developer signs an app under test. The bundle manager installs
# it only on the device with that UDID, which `innkeeper profile list` shows.
# The keys, certificates and profile template come from the SDK's
# toolchains/lib, and the key passwords are the SDK's published ones.
#
# usage: sign.sh <toolchains/lib dir> <bundle name> <udid> <in.hap> <out.hap>
set -euo pipefail
usage="usage: sign.sh <toolchains/lib dir> <bundle name> <udid> <in.hap> <out.hap>"
lib=${1:?$usage} bundle=${2:?$usage} udid=${3:?$usage} in=${4:?$usage} out=${5:?$usage}
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
# The profile names the bundle, the app certificate and the device. It is
# valid for ten years from yesterday.
node -e '
const fs = require("fs");
const [template, pem, bundle, udid, out] = process.argv.slice(1);
const p = JSON.parse(fs.readFileSync(template));
const leaf = fs.readFileSync(pem, "utf8").match(/-----BEGIN CERTIFICATE-----[^-]+-----END CERTIFICATE-----\n/)[0];
const now = Math.floor(Date.now() / 1000);
p.validity = { "not-before": now - 86400, "not-after": now + 3650 * 86400 };
p["bundle-info"]["bundle-name"] = bundle;
p["bundle-info"]["development-certificate"] = leaf;
p["debug-info"]["device-ids"] = [udid];
fs.writeFileSync(out, JSON.stringify(p, null, 2));
' "$lib/UnsgnedDebugProfileTemplate.json" "$work/app.pem" "$bundle" "$udid" "$work/profile.json"
sign sign-profile -keyAlias "openharmony application profile debug" -signAlg SHA256withECDSA \
  -mode localSign -profileCertFile "$lib/OpenHarmonyProfileDebug.pem" \
  -inFile "$work/profile.json" -outFile "$work/profile.p7b" "${pw[@]}"
sign sign-app -keyAlias "openharmony application release" -signAlg SHA256withECDSA -mode localSign \
  -appCertFile "$work/app.pem" -profileFile "$work/profile.p7b" -inFile "$in" -outFile "$out" \
  -signCode 1 "${pw[@]}"
