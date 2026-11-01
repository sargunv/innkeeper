# shellcheck shell=sh disable=SC2329 # functions run through retry
# Runs inside an OpenHarmony profile under OH's sh, as root with
# ohos.permission.DUMP (`innkeeper-session exec /system/bin/sh -s <boot.sh`),
# and waits for the core system abilities, screen 0 and SceneBoard.

# retry <tries> <command...> runs the command every 0.2 s until it succeeds.
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

samgr_ready() {
  /system/bin/param get bootevent.samgr.ready | grep -q true
}
if ! retry 300 samgr_ready; then
  echo "samgr not ready"
  exit 1
fi

# hidumper is a system ability that samgr starts on demand.
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

# Our composer (ohos/vendor/display) provides screen 0 through its ashmem
# command queue to the HDI. The screen has the panel mode that the launcher
# sets, <width>x<height>@<dpi>.
panel=$(/system/bin/param get const.innkeeper.display)
panel=${panel%% *}
has_screen() {
  screen=$(/system/bin/hidumper -s RenderService -a screen 2>&1)
  echo "$screen" | grep -q "screen\[0\]: id=0.*render resolution=${panel%@*}"
}
if ! retry 50 has_screen; then
  echo "no screen 0"
  echo "$screen"
  exit 1
fi

# SceneBoard is the home ability that AMS starts once user 100 is active. Its
# home screen is in front when its desktop window, SCBDesktop<n>, has input
# focus. SceneBoard sets bootevent.launcher.ready to true about 10 s later,
# when a timer runs out, because its desktop doesn't finish preloading its
# pages.
desktop_focused() {
  windows=$(/system/bin/hidumper -s WindowManagerService -a -a 2>&1)
  id=$(echo "$windows" | sed -n 's/^Focus window: //p')
  [ -n "$id" ] && echo "$windows" | grep -Eq "^SCBDesktop[0-9]* +[0-9]+ +[0-9]+ +$id "
}
if ! retry 150 desktop_focused; then
  echo "SceneBoard's home screen isn't in front: $(/system/bin/pidof com.ohos.sceneboard || echo not running)"
  echo "$windows"
  exit 1
fi

# Settings sets a PIN through the PIN and user authentication HDIs, which
# fail without their data directories.
for d in pinauth userauth; do
  if [ ! -d "/data/chipset/el1/public/$d" ]; then
    echo "no /data/chipset/el1/public/$d"
    exit 1
  fi
done

# DMS lays the screen out at the panel's dpi.
dpi=$(/system/bin/param get persist.window.screen.dpi)
dpi=${dpi%% *}
[ "$dpi" = "${panel#*@}" ] && exit 0
echo "DMS at $dpi dpi, the panel at ${panel#*@}"
exit 1
