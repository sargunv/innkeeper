# shellcheck shell=sh disable=SC2329 # functions run through retry
# Runs inside an OpenHarmony profile under OH's sh, as root, while Settings is
# in front (`innkeeper-session exec /system/bin/sh -s <pid> <render.sh`). <pid>
# is render_service's pid after boot. It flings Settings' list and goes home
# with OH's input injector, and checks that render_service has drawn every
# animation since boot without a restart (#309).
rs=$1

# retry <tries> <command...> runs the command every 0.1 s until it succeeds.
retry() {
  n=$1
  shift
  while [ "$n" -gt 0 ]; do
    "$@" && return 0
    n=$((n - 1))
    sleep 0.1
  done
  return 1
}

# A render_service crash also restarts SceneBoard, so each check of the screen
# checks render_service's pid first.
same_render_service() {
  now=$(/system/bin/pidof render_service)
  [ "$now" = "$rs" ] && return 0
  echo "render_service restarted: pid $rs, now ${now:-none}"
  exit 1
}

# Prints the name of the window with input focus, such as SCBDesktop6.
focused() {
  windows=$(/system/bin/hidumper -s WindowManagerService -a -a 2>&1)
  id=$(echo "$windows" | sed -n 's/^Focus window: //p')
  echo "$windows" | while read -r name _ _ win _; do
    if [ "$win" = "$id" ]; then
      echo "$name"
      break
    fi
  done
}
focus_is() {
  same_render_service
  case $(focused) in "$1"*) return 0 ;; esac
  return 1
}

if ! retry 15 focus_is settings; then
  echo "Settings is not in front: $(focused) has focus"
  exit 1
fi
# Flinging the list draws frames back to back, which is when the crash in
# #309 came within a few seconds.
n=5
while [ "$n" -gt 0 ]; do
  /system/bin/uinput -T -m 360 1100 360 300 100 >/dev/null 2>&1
  /system/bin/uinput -T -m 360 300 360 1100 100 >/dev/null 2>&1
  same_render_service
  n=$((n - 1))
done
# A swipe up from the bottom edge goes home. SceneBoard drops it if the list
# is still animating, so it repeats until the home screen has focus.
n=5
while [ "$n" -gt 0 ]; do
  /system/bin/uinput -T -m 360 1270 360 600 200 >/dev/null 2>&1
  retry 15 focus_is SCBDesktop && exit 0
  n=$((n - 1))
done
echo "no home screen: $(focused) has focus"
exit 1
