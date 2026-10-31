#!/usr/bin/env bash
# derive.sh relaxes unreadable partition files and gives clatd its exact mode
# and owner record.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$repo/out/test/derive
rm -rf "$work"
mkdir -p "$work/img/system/system/bin" "$work/img/vendor/etc" "$work/apex/com.android.tethering/bin/for-system"
trap 'chmod -R u+rwx "$work" 2>/dev/null; rm -rf "$work"' EXIT
install -m 0550 /dev/null "$work/img/system/system/bin/logd-tool"
install -m 0644 /dev/null "$work/img/vendor/etc/readable"
install -m 0755 /dev/null "$work/apex/com.android.tethering/bin/for-system/clatd"
"$repo/image/derive.sh" "$work/img" "$work/apex" "$work/out"
mode() { stat -c %a "$work/out/$1"; }
owner() { python3 -Ic 'import os, sys; print(os.getxattr(sys.argv[1], "user.innkeeper.owner").decode())' "$work/out/$1"; }
[[ $(mode system/bin/logd-tool) == 555 ]]
[[ ! -e $work/out/vendor/etc/readable ]]
[[ $(mode apex/com.android.tethering/bin/for-system) == 750 ]]
[[ $(owner apex/com.android.tethering/bin/for-system) == 1029:1000 ]]
[[ $(mode apex/com.android.tethering/bin/for-system/clatd) == 6755 ]]
[[ $(owner apex/com.android.tethering/bin/for-system/clatd) == 1029:1029 ]]
echo 'ok   derive relaxes unreadable files; clatd keeps 06755 and its owner record'
