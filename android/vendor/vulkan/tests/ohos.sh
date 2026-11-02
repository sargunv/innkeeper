#!/usr/bin/env bash
# Run ohos_swapchain_test and ohos_gles_test (built by build:vulkan into
# out/build/vulkan/test-ohos) in a fresh OpenHarmony profile, on the RADV or ANV
# that the launcher selects. ohos_gles_test loads our EGL, GLES and Zink from a
# copy in /data/local/tmp.
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
tests=$repo/out/build/vulkan/test-ohos
gl=$repo/out/build/vulkan/install/share/innkeeper/ohos/gpu/vendor/lib64/chipsetsdk
[[ -x $tests/ohos_gles_test && -f $gl/libEGL_impl.so ]] || {
  echo "no OpenHarmony test programs or GL libraries (run \`mise run build:vulkan\`)" >&2
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
push() { "$S" exec /system/bin/sh -c "cat >'$2' && chmod 755 '$2'" <"$1"; }
t=/data/local/tmp/innkeeper-gl
"$S" exec /system/bin/mkdir -p "$t"
push "$tests/ohos_swapchain_test" "$t/ohos_swapchain_test"
push "$tests/ohos_gles_test" "$t/ohos_gles_test"
for lib in libEGL_impl.so libGLESv2_impl.so libgallium-26.2.5.so; do push "$gl/$lib" "$t/$lib"; done
"$S" exec "$t/ohos_swapchain_test" | tee "$log/swapchain.log"
"$S" exec /system/bin/sh -c "LD_LIBRARY_PATH=$t $t/ohos_gles_test $t/libEGL_impl.so" | tee "$log/gles.log"
if ! grep -q -E '^device: .*(RADV|Intel)' "$log/swapchain.log" ||
  ! grep -q -E '^renderer: zink .*(RADV|Intel)' "$log/gles.log"; then
  echo "FAIL: the tests did not run on RADV or ANV" >&2
  exit 1
fi
grep -q '^PASS' "$log/swapchain.log" && grep -q '^PASS' "$log/gles.log"
