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
export INNKEEPER_PROFILE=ci-$mode
# nested.sh changes HOME and XDG dirs before invoking the task.
export MISE_DATA_DIR=${MISE_DATA_DIR:-$HOME/.local/share/mise}
export MISE_STATE_DIR=${MISE_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/mise}
restore_baseline() {
  git restore --source=HEAD -- tests/compat/results.json
}
trap restore_baseline EXIT
restore_baseline
status=0
tests/nested.sh mise run test:compat -- "${args[@]}" || status=$?
dest=out/ci/reports/$mode
mkdir -p "$dest"
# The APK cache is large and contains third-party apps; upload only results.
results=out/test/compat/$INNKEEPER_PROFILE
if [[ -d $results ]]; then cp -a "$results/." "$dest/"; fi
exit "$status"
