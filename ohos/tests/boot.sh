# shellcheck shell=sh disable=SC2329 # functions run through retry
# Runs inside the OpenHarmony session with OH's own sh (ohos/run.py --probe),
# once samgr is ready.

# retry <tries> <command...>: every 0.2 s until it succeeds.
retry() {
  n=$1
  shift
  while [ "$n" -gt 0 ]; do
    "$@" && return 0
    n=$((n - 1))
    sleep 0.2
  done
  return 1
}

# hidumper is itself a system ability samgr starts on demand.
want="SystemAbilityManager AccessTokenManagerService BundleMgr AbilityManagerService RenderService"
registered() {
  list=$(/system/bin/hidumper -ls 2>&1)
  missing=""
  for sa in $want; do
    echo "$list" | grep -qw "$sa" || missing="$missing $sa"
  done
  [ -z "$missing" ]
}
if ! retry 150 registered; then
  echo "not registered:$missing"
  echo "$list"
  exit 1
fi
echo "$list"

# Screen 0 comes from our composer (ohos/vendor/display), over its ashmem
# command queue to the HDI.
has_screen() {
  screen=$(/system/bin/hidumper -s RenderService -a screen 2>&1)
  echo "$screen" | grep -q "screen\[0\]: id=0.*render resolution=720x1280"
}
if ! retry 50 has_screen; then
  echo "no screen 0"
  echo "$screen"
  exit 1
fi

# SceneBoard, the home ability AMS starts once user 100 is active, sets this
# once its desktop has loaded.
launcher_ready() {
  /system/bin/param get bootevent.launcher.ready | grep -q true
}
if ! retry 200 launcher_ready; then
  echo "SceneBoard not ready: $(/system/bin/pidof com.ohos.sceneboard || echo not running)"
  exit 1
fi

# DMS lays the screen out at the composer's dpi (ohos/sandbox.sh).
dpi=$(/system/bin/param get persist.window.screen.dpi)
dpi=${dpi%% *}
[ "$dpi" = "${INNKEEPER_OHOS_DISPLAY#*@}" ] && exit 0
echo "DMS at $dpi dpi, the composer at ${INNKEEPER_OHOS_DISPLAY#*@}"
exit 1
