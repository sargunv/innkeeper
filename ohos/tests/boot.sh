# shellcheck shell=sh
# Runs inside the OpenHarmony session with OH's own sh (ohos/run.py --probe),
# once samgr is ready: OH's core system abilities register with samgr and
# hidumper (itself a system ability samgr starts on demand) lists them, then
# render_service has the display.
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
  echo "$screen" | grep -q "screen\[0\]: id=0.*render resolution=720x1280" && exit 0
  i=$((i + 1))
  sleep 0.2
done
echo "no screen 0"
echo "$screen"
exit 1
