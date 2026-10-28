#!/usr/bin/env bash
# The end-to-end suites, against one headless test instance (LINUXAPK_INSTANCE,
# default e2e) that this starts and stops: audio (which starts it, routed to a
# private null sink), system-app, then cli. Runs every suite, then fails if any
# did.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-e2e}
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
trap '"$session" stop >/dev/null' EXIT
failed=()
for suite in audio system-app cli; do
  echo "=== e2e: $suite"
  "$here/$suite/run.sh" || failed+=("$suite")
done
if ((${#failed[@]})); then
  echo "e2e: FAIL (${failed[*]})"
  exit 1
fi
echo "e2e: PASS"
