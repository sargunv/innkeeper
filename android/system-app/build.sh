#!/usr/bin/env bash
# Builds "Linux.apk System" (dev.linuxapk.system), runs its JVM unit tests,
# signs it with the platform key and installs it into out/system_ext-overlay,
# which the session layers over /system_ext:
#   priv-app/LinuxApkSystem/LinuxApkSystem.apk
#   etc/permissions/dev.linuxapk.system.xml
#
#   android/system-app/build.sh            # test + build + install
#   android/system-app/build.sh --no-test
#
# Signs with AOSP's public test-keys platform key
# (build/make/target/product/security/platform.{pk8,x509.pem}), which signs the
# emulator image and our userdebug builds. It's taken from the AOSP checkout
# ($AOSP, default ~/aosp/a17) or fetched from android.googlesource.com, and
# verified by SHA-256 either way. It isn't committed here. Override with
# LINUXAPK_PLATFORM_KEY=<path without suffix>.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${LINUXAPK_OUT:-$repo/out}
export ANDROID_HOME=${ANDROID_HOME:-$HOME/Android/Sdk}
bt=$ANDROID_HOME/build-tools/37.0.0
key=${LINUXAPK_PLATFORM_KEY:-}
if [[ -z $key ]]; then
  key=$out/keys/platform
  declare -A sums=(
    ['pk8']=1ad8ef556870edb70f69a9d3c112544c07de5162ba440d84d33f8bb0c5962875
    ['x509.pem']=9837de028f460c35cc8d3fa45f14eecce30f6fbfe4b93d399aef1acb80c20d14
  )
  mkdir -p "$out/keys"
  for ext in pk8 x509.pem; do
    f=$key.$ext
    if ! echo "${sums[$ext]}  $f" | sha256sum -c --status 2>/dev/null; then
      src=${AOSP:-$HOME/aosp/a17}/build/make/target/product/security/platform.$ext
      if [[ -f $src ]]; then
        cp "$src" "$f"
      else
        curl -sfL "https://android.googlesource.com/platform/build/+/refs/tags/android-17.0.0_r1/target/product/security/platform.$ext?format=TEXT" |
          base64 -d >"$f"
      fi
      echo "${sums[$ext]}  $f" | sha256sum -c --status ||
        {
          echo "platform.$ext: checksum mismatch" >&2
          exit 1
        }
    fi
  done
fi

tasks=(:protocol:test :app:assembleRelease)
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
