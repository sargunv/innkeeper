#!/usr/bin/env bash
# Regression: removing QuickStep disables Android's unimplemented recents proxy.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
launcher=$repo/android/overlay/system_ext/priv-app/Launcher3QuickStep/Launcher3QuickStep.apk
[[ -f $launcher && ! -s $launcher ]]
