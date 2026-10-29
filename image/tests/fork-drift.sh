#!/usr/bin/env bash
# Run the real check on the pinned cache, then prove changed and missing bases
# all fail together without ever writing the cache.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
img=$("$session" image | sed -n 's/^img=//p')
apex=$("$session" image | sed -n 's/^apex=//p')
if [[ -d $img/system/system && -f $apex/apex-info-list.xml ]]; then
  "$repo/image/check-forks.sh" "$img" "$apex" "$repo/image/fork-bases.sha256" "$repo/android/overlay"
  echo 'ok   all nine fork bases match the pinned image'
else
  echo 'skip pinned fork bases: prepare the image first (prepare always checks them)'
fi
work=$repo/out/test/fork-drift
mkdir -p "$work/img/system/system/etc" "$work/apex" "$work/overlay/system/etc"
trap 'rm -rf "$work"' EXIT
printf 'base\n' >"$work/img/system/system/etc/test.rc"
printf 'overlay\n' >"$work/overlay/system/etc/test.rc"
hash=$(sha256sum <"$work/img/system/system/etc/test.rc")
printf '%s  system/etc/test.rc\n' "${hash%% *}" >"$work/bases"
"$repo/image/check-forks.sh" "$work/img" "$work/apex" "$work/bases" "$work/overlay"
printf 'changed\n' >"$work/img/system/system/etc/test.rc"
printf '%s  apex/missing.rc\n' "${hash%% *}" >>"$work/bases"
if "$repo/image/check-forks.sh" "$work/img" "$work/apex" "$work/bases" "$work/overlay" >"$work/log" 2>&1; then
  echo 'FAIL drift was accepted' >&2
  exit 1
fi
grep -q 'image fork drift: system/etc/test.rc' "$work/log"
grep -q 'image fork drift: apex/missing.rc' "$work/log"
grep -q '^+overlay' "$work/log"
echo 'ok   unchanged bases pass; changed and missing bases fail together with a diff'
