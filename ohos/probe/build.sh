#!/usr/bin/env bash
# build:ohos-probe: the unsigned InnkeeperProbes.hap, the app that the ohos e2e
# group installs, into out/test/probes. ohos/hap.sh builds it from this
# directory's sources. A HAP installs only once it is signed for the device,
# so the test signs it with ohos/sign.sh for its profile's UDID. The build
# copies the SDK files that sign.sh uses to out/test/probes/ohos-sign, and the
# SDK's hdc client, which the test connects with, to out/test/probes/hdc.
set -euo pipefail
top=$(cd "$(dirname "$0")/../.." && pwd)
cd "$top"
sdk=$(ohos/sdk.sh)
mkdir -p out/test/probes
ohos/hap.sh "$sdk" ohos/probe out/build/ohos-probe out/test/probes/InnkeeperProbes.hap
kit=out/test/probes/ohos-sign
rm -rf "$kit"
mkdir -p "$kit"
cp "$sdk"/toolchains/lib/{hap-sign-tool.jar,OpenHarmony.p12,OpenHarmonyProfileDebug.pem,UnsgnedDebugProfileTemplate.json} "$kit/"
hdc=out/test/probes/hdc
rm -rf "$hdc"
mkdir -p "$hdc"
cp "$sdk"/toolchains/{hdc,libusb_shared.so} "$hdc/"
