# shellcheck shell=sh
# Runs inside the OpenHarmony session with OH's own sh (ohos/run.py --probe),
# once samgr is ready: OH's core system abilities register with samgr and
# hidumper (itself a system ability samgr starts on demand) lists them, then
# render_service has the display, then SceneBoard has loaded its desktop at
# the composer's density.
want="SystemAbilityManager AccessTokenManagerService BundleMgr AbilityManagerService RenderService"
i=0
while [ $i -lt 150 ]; do
  list=$(/system/bin/hidumper -ls 2>&1)
  missing=""
  for sa in $want; do
    echo "$list" | grep -qw "$sa" || missing="$missing $sa"
  done
  if [ -z "$missing" ]; then
    echo "$list"
    break
  fi
  i=$((i + 1))
  sleep 0.2
done
if [ -n "$missing" ]; then
  echo "not registered:$missing"
  echo "$list"
  exit 1
fi
# render_service has screen 0 from our composer (ohos/vendor/display), which
# needs its ashmem command queue to the HDI.
i=0
while [ $i -lt 50 ]; do
  screen=$(/system/bin/hidumper -s RenderService -a screen 2>&1)
  echo "$screen" | grep -q "screen\[0\]: id=0.*render resolution=720x1280" && break
  i=$((i + 1))
  sleep 0.2
done
if [ $i -eq 50 ]; then
  echo "no screen 0"
  echo "$screen"
  exit 1
fi
# SceneBoard, OH's window manager and home screen, is the home ability AMS
# starts once user 100 is active; it sets this once its desktop has loaded.
i=0
while [ $i -lt 200 ]; do
  /system/bin/param get bootevent.launcher.ready | grep -q true && break
  i=$((i + 1))
  sleep 0.2
done
if [ $i -eq 200 ]; then
  echo "SceneBoard not ready: $(/system/bin/pidof com.ohos.sceneboard || echo not running)"
  exit 1
fi
# DMS lays the screen out at the composer's dpi (ohos/sandbox.sh).
dpi=$(/system/bin/param get persist.window.screen.dpi)
dpi=${dpi%% *}
[ "$dpi" = "${LINUXAPK_OHOS_DISPLAY#*@}" ] && exit 0
echo "DMS at $dpi dpi, the composer at ${LINUXAPK_OHOS_DISPLAY#*@}"
exit 1
