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
  journalctl --user -u "$unit" --no-pager -n 200 >"$N/compositor.log" 2>&1 || true
  # A deadline can interrupt the runner before its trap reaches session stop.
  # Flatpak's scope is outside this desktop's service; reap that test scope too.
  if [[ ${LINUXAPK_E2E:-} == 1 && -f $repo/out/dev/flatpak/$LINUXAPK_INSTANCE/cgroup ]]; then
    local cg
    cg=$(cat "$repo/out/dev/flatpak/$LINUXAPK_INSTANCE/cgroup")
    DBUS_SESSION_BUS_ADDRESS="unix:path=$host_runtime/bus" XDG_RUNTIME_DIR="$host_runtime" \
      systemctl --user stop "${cg##*/}" 2>/dev/null || true
  fi
  DBUS_SESSION_BUS_ADDRESS="unix:path=$host_runtime/bus" XDG_RUNTIME_DIR="$host_runtime" \
    systemctl --user stop "$unit" 2>/dev/null || true
  rm -rf "$rt"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$N"/{home,config,data,cache,state}
# KWin grants fake seat input only to an executable whose desktop file names
# the protocol. This entry lives solely on this disposable desktop.
mkdir -p "$N/data/applications"
cat >"$N/data/applications/dev.linuxapk.TestSeat.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Linux.apk test seat
Exec=$(readlink -f /usr/bin/python3)
NoDisplay=true
X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input
DESKTOP

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
if [ "${LINUXAPK_E2E:-}" = 1 ]; then
  /usr/bin/python3 "$repo/tests/e2e/desktop.py" "$N/data/picked.txt" >"$N/desktop.log" 2>&1 &
  gdbus wait --session --timeout 10 org.freedesktop.portal.Desktop
fi
/usr/libexec/at-spi-bus-launcher --launch-immediately --a11y=1 &
gdbus wait --session --timeout 10 org.a11y.Bus
/usr/libexec/at-spi2-registryd &
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
# Explicit renderer overrides belong to this desktop, not the user's manager.
renderer_env=()
for key in LIBGL_ALWAYS_SOFTWARE KWIN_COMPOSE; do
  if [[ -v $key ]]; then renderer_env+=("$key=${!key}"); fi
done
# systemd itself is reached through the host bus; everything it starts inherits
# the private runtime and the nested Wayland socket before dbus-run-session.
DBUS_SESSION_BUS_ADDRESS="unix:path=$host_runtime/bus" systemd-run --user --quiet --collect \
  --unit "$unit" -p MemoryMax=3G -- env -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
  LANG="$LANG" LANGUAGE="$LANGUAGE" HOME="$HOME" XDG_RUNTIME_DIR="$rt" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
  XDG_DATA_HOME="$XDG_DATA_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
  XDG_DATA_DIRS="$XDG_DATA_DIRS" XDG_CURRENT_DESKTOP=KDE FLATPAK_USER_DIR="$FLATPAK_USER_DIR" \
  PULSE_SERVER="$PULSE_SERVER" WAYLAND_DISPLAY=lapk-nested \
  "${renderer_env[@]}" \
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
gdbus call --session --dest org.kde.KWin --object-path /KWin \
  --method org.kde.KWin.supportInformation >"$N/kwin-support.txt"
if [[ ${GITHUB_ACTIONS:-} == true ]] && ! grep -q 'Compositing Type: OpenGL' "$N/kwin-support.txt"; then
  echo "nested KWin needs OpenGL and a DRM device; see $N/kwin-support.txt" >&2
  exit 1
fi
"$@"
