#!/usr/bin/env bash
# Builds "Linux.apk System" (dev.linuxapk.system), runs its JVM unit tests,
# signs it with the platform key and installs it into out/system_ext-overlay,
# which the session layers over /system_ext:
#   priv-app/LinuxApkSystem/LinuxApkSystem.apk
#   etc/permissions/dev.linuxapk.system.xml
#
#   android/build.sh            # test + build + install
#   android/build.sh --no-test
#
# keys/platform.{pk8,x509.pem} are AOSP's public test-keys platform key
# (build/make/target/product/security), which signs the emulator image and our
# userdebug builds. Override with LINUXAPK_PLATFORM_KEY=<path without suffix>.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
out=${LINUXAPK_OUT:-$repo/out}
export ANDROID_HOME=${ANDROID_HOME:-$HOME/Android/Sdk}
bt=$ANDROID_HOME/build-tools/37.0.0
key=${LINUXAPK_PLATFORM_KEY:-$here/keys/platform}

tasks=(:input:test :app:assembleRelease)
[[ ${1:-} == --no-test ]] && tasks=(:app:assembleRelease)

cd "$here"
mise exec -- ./gradlew --console=plain -q "${tasks[@]}"

apk=$here/app/build/outputs/apk/release/app-release-unsigned.apk
dst=$out/system_ext-overlay
app=$dst/priv-app/LinuxApkSystem
mkdir -p "$app" "$dst/etc/permissions"
"$bt/zipalign" -c -P 16 4 "$apk"
"$bt/apksigner" sign --key "$key.pk8" --cert "$key.x509.pem" \
  --out "$app/LinuxApkSystem.apk" "$apk"
rm -f "$app/LinuxApkSystem.apk.idsig"
"$bt/apksigner" verify --print-certs "$app/LinuxApkSystem.apk" | grep -m1 SHA-256
cp "$here/etc/permissions/dev.linuxapk.system.xml" "$dst/etc/permissions/"
chmod -R go+rX "$dst"
echo "installed $app/LinuxApkSystem.apk"
