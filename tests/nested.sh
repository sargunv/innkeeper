#!/usr/bin/env bash
# Run a command on a disposable KWin desktop with a private bus and XDG dirs.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
export LINUXAPK_IMAGES=${LINUXAPK_IMAGES:-${XDG_DATA_HOME:-$HOME/.local/share}/linuxapk/images}
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-nested}
N=$repo/out/test/nested/$LINUXAPK_INSTANCE
host_runtime=$XDG_RUNTIME_DIR
rt=$(mktemp -d -p "$host_runtime" lapk-XXXXXX)
unit=linuxapk-nested-$LINUXAPK_INSTANCE
cleanup() {
  DBUS_SESSION_BUS_ADDRESS="unix:path=$host_runtime/bus" XDG_RUNTIME_DIR="$host_runtime" \
    systemctl --user stop "$unit" 2>/dev/null || true
  rm -rf "$rt"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$N"/{home,config,data,cache,state}
if [[ -n ${LINUXAPK_TEST_LAUNCHER_PORTAL:-} ]]; then
  mkdir -p "$N/config/xdg-desktop-portal"
  printf '[preferred]\norg.freedesktop.impl.portal.DynamicLauncher=%s\n' \
    "$LINUXAPK_TEST_LAUNCHER_PORTAL" >"$N/config/xdg-desktop-portal/portals.conf"
fi
cat >"$N/hold.sh" <<HOLD
#!/bin/sh
echo "\$DBUS_SESSION_BUS_ADDRESS" >"$N/bus"
exec sleep infinity
HOLD
chmod +x "$N/hold.sh"
cat >"$N/compositor.sh" <<HOLD
#!/bin/sh
/usr/libexec/at-spi-bus-launcher --launch-immediately --a11y=1 &
sleep 1
/usr/libexec/at-spi2-registryd &
sleep 1
exec kwin_wayland --virtual --no-lockscreen --socket lapk-nested \\
  --width 1920 --height 1080 --exit-with-session "$N/hold.sh"
HOLD
chmod +x "$N/compositor.sh"
printf '%s\n' "$rt" >"$N/runtime"
rm -f "$N/bus"
export PULSE_SERVER=${PULSE_SERVER:-unix:$host_runtime/pulse/native}
export FLATPAK_USER_DIR=$repo/out/flatpak/user
export XDG_DATA_DIRS=$FLATPAK_USER_DIR/exports/share:/usr/local/share:/usr/share
export LANG=C.UTF-8 LANGUAGE=C
export HOME=$N/home XDG_CONFIG_HOME=$N/config XDG_DATA_HOME=$N/data
export XDG_CACHE_HOME=$N/cache XDG_STATE_HOME=$N/state XDG_CURRENT_DESKTOP=KDE
unset DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS
# systemd itself is reached through the host bus; everything it starts inherits
# the private runtime and the nested Wayland socket before dbus-run-session.
DBUS_SESSION_BUS_ADDRESS="unix:path=$host_runtime/bus" systemd-run --user --quiet --collect \
  --unit "$unit" -p MemoryMax=3G -- env -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
  LANG="$LANG" LANGUAGE="$LANGUAGE" HOME="$HOME" XDG_RUNTIME_DIR="$rt" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
  XDG_DATA_HOME="$XDG_DATA_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
  XDG_DATA_DIRS="$XDG_DATA_DIRS" XDG_CURRENT_DESKTOP=KDE FLATPAK_USER_DIR="$FLATPAK_USER_DIR" \
  PULSE_SERVER="$PULSE_SERVER" WAYLAND_DISPLAY=lapk-nested \
  dbus-run-session -- "$N/compositor.sh"
for _ in $(seq 100); do
  [[ -s $N/bus && -S $rt/lapk-nested ]] && break
  sleep 0.1
done
[[ -s $N/bus && -S $rt/lapk-nested ]] || {
  echo 'nested KWin did not start' >&2
  exit 1
}
DBUS_SESSION_BUS_ADDRESS=$(cat "$N/bus")
export XDG_RUNTIME_DIR=$rt WAYLAND_DISPLAY=lapk-nested DBUS_SESSION_BUS_ADDRESS
export LINUXAPK_NESTED=1
"$@"
