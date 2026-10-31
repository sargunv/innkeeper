#!/usr/bin/env bash
# Sourced by run.sh: real Android requests, production bridge policy, fake desktop.
# Keep the named launcher's policy intact. Existing display/audio connections
# stay on its test bridge; only new connections use this temporary sidecar.
# shellcheck disable=SC2154 # repo and desktop_log come from run.sh.
: "${repo:?}" "${desktop_log:?}"
socket=$XDG_RUNTIME_DIR/app/dev.linuxapk.Manager/linuxapk-$LINUXAPK_PROFILE/bridge/bridge.sock
unit=linuxapk-production-$LINUXAPK_PROFILE
host_runtime=/run/user/$(id -u)
production=$T/production
mkdir -p "$production"
host_command() {
  DBUS_SESSION_BUS_ADDRESS="unix:path=$host_runtime/bus" XDG_RUNTIME_DIR="$host_runtime" \
    "$@"
}
restore_test_bridge() {
  host_command systemctl --user stop "$unit" >/dev/null 2>&1 || true
  mv "$socket.test" "$socket"
  # Drop sidecar connections before another group (especially lifecycle).
  s kill -9 "$(s pidof dev.linuxapk.system)" || true
}
# Only tests/nested.sh supplies these private dirs and logging fake services.
[[ ${LINUXAPK_NESTED:-} == 1 && $XDG_RUNTIME_DIR != "$host_runtime" ]]
[[ $HOME == "$repo/out/"* && $XDG_STATE_HOME == "$repo/out/"* ]]
s am force-stop dev.linuxapk.probes
mv "$socket" "$socket.test"
trap restore_test_bridge EXIT
host_command systemd-run --user --quiet --collect --unit "$unit" -p MemoryMax=512M \
  -p "BindsTo=linuxapk-nested-$LINUXAPK_PROFILE.service" \
  -p "After=linuxapk-nested-$LINUXAPK_PROFILE.service" \
  -p "StandardOutput=append:$production/bridge.log" -p StandardError=inherit -- \
  env -u DISPLAY -u WAYLAND_DISPLAY -u LINUXAPK_TEST \
  HOME="$HOME" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
  XDG_CONFIG_HOME="$XDG_CONFIG_HOME" XDG_DATA_HOME="$XDG_DATA_HOME" \
  XDG_CACHE_HOME="$XDG_CACHE_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
  DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" \
  DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" \
  LINUXAPK_PROFILE="$LINUXAPK_PROFILE" \
  "$repo/out/stage/libexec/linuxapk/linuxapk-bridge" \
  --run-dir "$(dirname "$(dirname "$socket")")" --state-dir "$production" \
  --system-app-dir "${LINUXAPK_DATA:-$repo/out/dev/flatpak/$LINUXAPK_PROFILE/android-data}/data/dev.linuxapk.system" \
  --sink "$LINUXAPK_AUDIO_SINK" \
  >"$production/start.txt" 2>&1
until_ok test -S "$socket"
s kill -9 "$(s pidof dev.linuxapk.system)"

s am start -a android.intent.action.VIEW -d mailto:production-e2e@example.com >/dev/null
until_ok file_has "$desktop_log" '^openuri: mailto:production-e2e@example.com$'
s am start -n dev.linuxapk.probes/.NotificationProbe >/dev/null
until_ok file_has "$desktop_log" '^notification: Ada Lovelace *: Are we still on for the demo at 3\?'
# No window is needed to exercise the picker result on Android's default display.
s am start -n dev.linuxapk.probes/.DesktopProbe >/dev/null
until_ok file_has "$desktop_log" '^picker: Open File$'
until_ok log_has LinuxApkDesktopProbe 'picked: linuxapk-e2e-file'
wm CLOSE --es package dev.linuxapk.probes >/dev/null
echo 'ok   production VIEW, notification and PICK reached the private desktop fakes'
