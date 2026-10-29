#!/usr/bin/env bash
# Preserve each mode's report before the next run writes out/test/compat.
set -euo pipefail
mode=${1:?headless or windowed}
args=()
case "$mode" in
  headless) ;;
  windowed) args+=(--window) ;;
  *) exit 2 ;;
esac
export LINUXAPK_INSTANCE=ci-$mode
export LINUXAPK_DATA=$PWD/out/ci/desktop/android-data-$mode
restore_baseline() {
  git restore --source=HEAD -- tests/compat/results.json docs/compatibility.md
}
trap restore_baseline EXIT
restore_baseline
status=0
mise run test:compat -- "${args[@]}" || status=$?
dest=out/ci/reports/$mode
mkdir -p "$dest"
# The APK cache is large and contains third-party apps; upload only results.
find out/test/compat -mindepth 1 -maxdepth 1 ! -name corpus -exec cp -a -t "$dest" {} +
cp -a out/ci/desktop/state "$dest/session-state"
exit "$status"
