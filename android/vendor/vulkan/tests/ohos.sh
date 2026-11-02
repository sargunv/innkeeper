#!/usr/bin/env bash
# Run ohos_swapchain_test (built by build:vulkan into out/build/vulkan/test-ohos)
# in a fresh OpenHarmony profile, on the RADV or ANV that the launcher selects.
# Without a GPU, OpenHarmony has no Vulkan driver, and the test is skipped.
# Off a nested desktop, the script runs itself under tests/nested.sh, which the
# CLI needs to create the profile.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
export INNKEEPER_PROFILE=${INNKEEPER_PROFILE:-vulkan-ohos}
if [[ ${INNKEEPER_NESTED:-} != 1 ]]; then
  exec "$repo/tests/nested.sh" "$0" "$@"
fi
test=$repo/out/build/vulkan/test-ohos/ohos_swapchain_test
[[ -x $test ]] || {
  echo "no $test (run \`mise run build:vulkan\`)" >&2
  exit 1
}
S=$repo/tools/dev/session.sh
cli=$repo/tools/dev/innkeeper.sh
log=$repo/out/test/vulkan-ohos/$INNKEEPER_PROFILE
mkdir -p "$log"
cleanup() {
  "$S" stop >/dev/null 2>&1 || true
  "$cli" profile delete "$INNKEEPER_PROFILE" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
"$cli" profile create --system ohos "$INNKEEPER_PROFILE" >/dev/null
"$S" start --headless >"$log/start.log" 2>&1
booted() { "$S" exec /system/bin/param get bootevent.boot.completed 2>/dev/null | grep -q '^true'; }
deadline=$((SECONDS + 120))
until booted; do
  ((SECONDS < deadline)) || {
    echo "FAIL: OpenHarmony did not boot (see $log)" >&2
    exit 1
  }
done
# The broker lets the guest open only the render node that the launcher
# selected. The guest lists every node.
# shellcheck disable=SC2016 # the guest's sh expands it
if ! "$S" exec /system/bin/sh -c 'for n in /dev/dri/renderD*; do (: <"$n") 2>/dev/null && exit 0; done; exit 1'; then
  echo "skip: the launcher selected no GPU, so OpenHarmony has no Vulkan driver"
  exit 0
fi
"$S" exec /system/bin/sh -c \
  'cat >/data/local/tmp/ohos_swapchain_test && chmod 755 /data/local/tmp/ohos_swapchain_test' <"$test"
"$S" exec /data/local/tmp/ohos_swapchain_test | tee "$log/test.log"
grep -q -E '^device: .*\((RADV|Intel)' "$log/test.log" || {
  echo "FAIL: the test did not run on RADV or ANV" >&2
  exit 1
}
grep -q '^PASS' "$log/test.log"
