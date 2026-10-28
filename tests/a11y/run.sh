#!/usr/bin/env bash
# Android apps on the AT-SPI bus (host/bridge/src/a11y/), end to end: a nested,
# isolated KWin with its own session bus and AT-SPI bus
# (docs/development.md#a-nested-isolated-desktop), switched on as an assistive
# technology does (org.a11y.Status IsEnabled), and a test session on it that
# opens the probes' A11yProbeActivity. atspi.py then checks it with pyatspi.
# Needs kwin_wayland, at-spi2-core and pyatspi (/usr/bin/python3).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-a11y}
T=$repo/out/test/a11y/$LINUXAPK_INSTANCE
export LINUXAPK_DATA=${LINUXAPK_DATA:-$T/android-data}
N=$T/desktop # the nested desktop's runtime and XDG dirs
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
probes=$repo/out/test/probes/LinuxApkProbes.apk
unit=linuxapk-a11y-kwin-$LINUXAPK_INSTANCE
sock=lapk-a11y-$LINUXAPK_INSTANCE
[[ -x $session && -f $probes ]] || {
  echo "no $session or $probes (run \`mise run build\`)" >&2
  exit 2
}

cleanup() {
  "$session" stop >/dev/null 2>&1 || true
  systemctl --user stop "$unit" 2>/dev/null || true
  [[ -z ${rt:-} ]] || rm -rf "$rt"
}
trap cleanup EXIT
cleanup
rm -rf "$N"
mkdir -p "$N"/{home,config,data,cache,state}
# Short, so the Wayland socket's path fits in 108 bytes from a deep checkout.
rt=$(mktemp -d -p "$XDG_RUNTIME_DIR" lapk-XXXXXX)
# The session's command: starts the AT-SPI bus and its registry (D-Bus activation can't start
# them here: SELinux refuses the first, and the second goes through systemd), publishes the
# nested bus, then waits.
cat >"$N/hold.sh" <<HOLD
#!/bin/sh
/usr/libexec/at-spi-bus-launcher --launch-immediately --a11y=1 &
sleep 1
/usr/libexec/at-spi2-registryd &
echo "\$DBUS_SESSION_BUS_ADDRESS" >$N/bus
exec sleep infinity
HOLD
chmod +x "$N/hold.sh"
E=(-u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u AT_SPI_BUS_ADDRESS
  HOME="$N/home" XDG_RUNTIME_DIR="$rt" XDG_CONFIG_HOME="$N/config" XDG_DATA_HOME="$N/data"
  XDG_CACHE_HOME="$N/cache" XDG_STATE_HOME="$N/state" XDG_CURRENT_DESKTOP=KDE)
systemd-run --user --quiet --collect --unit "$unit" -p MemoryMax=3G -- env "${E[@]}" \
  WAYLAND_DISPLAY="$sock" dbus-run-session -- kwin_wayland --virtual --no-lockscreen \
  --socket "$sock" --width 1280 --height 800 --exit-with-session "$N/hold.sh"
for _ in $(seq 100); do
  [[ -s $N/bus && -S $rt/$sock ]] && break
  sleep 0.1
done
[[ -s $N/bus ]] || {
  echo "FAIL nested KWin did not start" >&2
  exit 1
}
bus=$(cat "$N/bus")
# What an assistive technology does when it starts (true) and when it ends (false).
a11y_enabled() {
  env "${E[@]}" DBUS_SESSION_BUS_ADDRESS="$bus" dbus-send --session --dest=org.a11y.Bus \
    --type=method_call --print-reply /org/a11y/bus org.freedesktop.DBus.Properties.Set \
    string:org.a11y.Status string:IsEnabled "variant:boolean:$1" >/dev/null
}
a11y_enabled true

export WAYLAND_DISPLAY=$rt/$sock DBUS_SESSION_BUS_ADDRESS=$bus
unset DISPLAY AT_SPI_BUS_ADDRESS
"$session" reset >/dev/null
"$session" start >/dev/null
t=$SECONDS
until [[ $("$session" exec getprop sys.boot_completed 2>/dev/null) == 1 ]]; do
  ((SECONDS - t < 300)) || {
    echo "FAIL not booted after 300 s" >&2
    exit 1
  }
  sleep 2
done
echo "ok   booted in $((SECONDS - t)) s"
s() { "$session" exec "$@"; }
size=$(stat -c %s "$probes")
s cmd package install -r -g -S "$size" <"$probes" >/dev/null
out=$(s am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LAUNCH \
  --es package dev.linuxapk.probes --es component dev.linuxapk.probes/.A11yProbeActivity)
grep -q "launched display=" <<<"$out" || {
  echo "FAIL launch: $out" >&2
  exit 1
}
label=$(sed -n 's/.*label=//p' <<<"$(s am broadcast -n dev.linuxapk.system/.wm.ControlReceiver \
  -a dev.linuxapk.LIST)" | head -1 | sed 's/"$//')
env "${E[@]}" DBUS_SESSION_BUS_ADDRESS="$bus" /usr/bin/python3 -I "$here/atspi.py" "$label" || {
  s logcat -d -t 40 -s LinuxApkA11y:V LinuxApkBridge:V
  grep "a11y" "${XDG_STATE_HOME:-$HOME/.local/state}/linuxapk-$LINUXAPK_INSTANCE/bridge.log" | tail -20
  exit 1
}
# The Android service follows the desktop's switch.
services() { s settings get secure enabled_accessibility_services | tr -d '\r'; }
if ! grep -q HostAccessibilityService <<<"$(services)"; then
  echo "FAIL Android service on: $(services)"
  exit 1
fi
echo "ok   Android service on"
a11y_enabled false
for _ in $(seq 20); do
  grep -q HostAccessibilityService <<<"$(services)" || break
  sleep 0.5
done
if grep -q HostAccessibilityService <<<"$(services)"; then
  echo "FAIL Android service off once the desktop's is: $(services)"
  exit 1
fi
echo "ok   Android service off once the desktop's is"
