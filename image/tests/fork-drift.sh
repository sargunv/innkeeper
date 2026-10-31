#!/usr/bin/env bash
# Unchanged overlay bases pass; changed and missing bases fail together.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
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
printf '%s  system/etc/test.rc  system_ext/etc/init/ours.rc\n' "${hash%% *}" >>"$work/bases"
if "$repo/image/check-forks.sh" "$work/img" "$work/apex" "$work/bases" "$work/overlay" >"$work/log" 2>&1; then
  echo 'FAIL drift was accepted' >&2
  exit 1
fi
grep -q 'image fork drift: system/etc/test.rc' "$work/log"
grep -q 'image fork drift: apex/missing.rc' "$work/log"
grep -q '^+overlay' "$work/log"
grep -q 'overridden in overlay/system_ext/etc/init/ours.rc' "$work/log"
echo 'ok   unchanged bases pass; changed and missing bases fail together with a diff or the override file'
