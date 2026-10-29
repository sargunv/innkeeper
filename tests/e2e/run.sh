#!/usr/bin/env bash
# The end-to-end suites, against one isolated windowed test instance (LINUXAPK_INSTANCE,
# default e2e) that this starts and stops: audio (which starts it, routed to a
# private null sink), system-app, then cli. Runs every suite, then fails if any
# did.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-e2e}
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then
  mise run test:compat -- fetch --only io.anuke.mindustry,com.darkempire78.opencalculator,org.wikipedia
  export LINUXAPK_TEST_LAUNCHER_PORTAL=${LINUXAPK_TEST_LAUNCHER_PORTAL:-gtk}
  exec "$repo/tests/nested.sh" "$0" "$@"
fi
session=$repo/tools/dev/session.sh
"$session" stop >/dev/null
"$session" reset >/dev/null
trap '"$session" stop >/dev/null; "$session" reset >/dev/null' EXIT
trap 'exit 130' INT TERM
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
