#!/usr/bin/env bash
# Host state in Android (#19, #16) against stand-ins for the desktop's
# services, so nothing reads the user's real battery, network or location:
# a private system bus with python-dbusmock's upower, power_profiles_daemon
# and networkmanager templates, and a private session bus with
# mock-location-portal.py. Neither bus can activate services. Boots a fresh
# headless test instance (LINUXAPK_INSTANCE, default hoststate; /data at
# LINUXAPK_DATA, default out/test/hoststate/data) with them and checks what
# Android shows:
#   tests/hoststate/run.sh [--deny] [--keep]
# --deny makes the portal answer the location request with "no"; --keep
# leaves the session and the mocks running (stop them with Ctrl-C).
# Needs python-dbusmock for /usr/bin/python3 (or LINUXAPK_DBUSMOCK_PYTHON).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
session=$repo/tools/dev/session.sh
probes=$repo/out/test/probes/LinuxApkProbes.apk
py=${LINUXAPK_DBUSMOCK_PYTHON:-/usr/bin/python3}
deny="" keep=""
for a in "$@"; do
  case $a in
    --deny) deny=--deny ;;
    --keep) keep=1 ;;
    *)
      echo "usage: $0 [--deny] [--keep]" >&2
      exit 2
      ;;
  esac
done
[[ -x $session && -f $probes ]] || {
  echo "no $session or $probes (run \`mise run build\`)" >&2
  exit 2
}
"$py" -c 'import dbusmock' 2>/dev/null || {
  echo "python-dbusmock is missing for $py (set LINUXAPK_DBUSMOCK_PYTHON)" >&2
  exit 2
}

T=$repo/out/test/hoststate
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-hoststate}
export LINUXAPK_DATA=${LINUXAPK_DATA:-$T/data}
"$session" stop >/dev/null 2>&1 || true
"$session" reset >/dev/null # /data holds subordinate ids: only reset deletes it
find "$T" -mindepth 1 -maxdepth 1 ! -name data -exec rm -rf {} + 2>/dev/null || true
mkdir -p "$T/rt" "$T/config"
chmod 700 "$T/rt"

pids=()
# shellcheck disable=SC2329 # run by the EXIT trap
cleanup() {
  "$session" stop >/dev/null 2>&1 || true
  "$session" reset >/dev/null 2>&1 || true
  ((${#pids[@]})) && kill "${pids[@]}" 2>/dev/null
  [[ -z ${rt:-} ]] || rm -rf "$rt"
  return 0
}
trap cleanup EXIT

# Two buses that can't activate anything (no service dirs), with a private
# runtime dir for whatever runs on them.
for b in system session; do
  cat >"$T/$b.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=$T/$b.sock</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
  env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR="$T/rt" \
    dbus-daemon --config-file="$T/$b.conf" --fork --print-pid=3 3>"$T/$b.pid"
  pids+=("$(<"$T/$b.pid")")
done
sys=unix:path=$T/system.sock
export DBUS_SYSTEM_BUS_ADDRESS=$sys DBUS_SESSION_BUS_ADDRESS=unix:path=$T/session.sock
mock() {
  XDG_RUNTIME_DIR="$T/rt" "$@" &
  pids+=($!)
}
for t in upower power_profiles_daemon networkmanager; do
  mock "$py" -m dbusmock --template "$t" >"$T/mock-$t.log" 2>&1
done
mock /usr/bin/python3 "$here/mock-location-portal.py" $deny >"$T/portal.log" 2>&1
sleep 2
call() { gdbus call -a "$1" -d "$2" -o "$3" -m "$4" "${@:5}" >/dev/null; }
call "$sys" org.freedesktop.NetworkManager /org/freedesktop/NetworkManager \
  org.freedesktop.DBus.Mock.AddProperty org.freedesktop.NetworkManager Metered '<uint32 2>'
set_prop() { call "$sys" "$1" "$2" org.freedesktop.DBus.Properties.Set "$3" "$4" "$5"; }

# The desktop's locale (KDE): plasma-localerc, edited below while running.
export XDG_CURRENT_DESKTOP=KDE XDG_CONFIG_HOME=$T/config LANG=en_US.UTF-8
unset LANGUAGE LC_ALL LC_MESSAGES LC_TIME LC_MEASUREMENT
printf '[Formats]\nLANG=en_US.UTF-8\n' >"$T/config/plasma-localerc"

fail=0
ok() { echo "ok   $1"; }
bad() {
  echo "FAIL $1"
  fail=1
}
s() { "$session" exec "$@" | tr -d '\r'; }
# name, command (run until it prints a line matching regex, up to 20 s), regex
wait_for() {
  local t=$SECONDS out
  while :; do
    out=$(eval "$2" 2>&1 || true)
    if grep -qE "$3" <<<"$out"; then
      ok "$1"
      return
    fi
    ((SECONDS - t < 20)) || break
    sleep 1
  done
  bad "$1"
  grep -E "${4:-.}" <<<"$out" | head -5 | sed 's/^/     /'
}

rt=$(mktemp -d -p "$XDG_RUNTIME_DIR" lapk-XXXXXX)
export XDG_RUNTIME_DIR=$rt
"$session" start --headless >/dev/null
t=$SECONDS
until [[ $(s getprop sys.boot_completed 2>/dev/null) == 1 ]]; do
  "$session" status >/dev/null || {
    echo "FAIL the session stopped while booting" >&2
    exit 1
  }
  ((SECONDS - t < 300)) || {
    echo "FAIL not booted after 300 s" >&2
    exit 1
  }
  sleep 2
done
ok "booted in $((SECONDS - t)) s"

# Battery: the Health HAL, fed by UPower.
wait_for "health HAL registered" "s logcat -d -s linuxapk-health:I" "registered .*IHealth/default"
wait_for "no battery: AC powered and full" "s dumpsys battery" "AC powered: true" "AC powered|present|level"
wait_for "no battery: not present, level 100" "s dumpsys battery" "present: false" "present|level"
call "$sys" org.freedesktop.UPower /org/freedesktop/UPower \
  org.freedesktop.DBus.Mock.SetupDisplayDevice 2 2 42.0 40.0 80.0 10.0 3600 0 true battery 1
set_prop org.freedesktop.UPower /org/freedesktop/UPower org.freedesktop.UPower OnBattery '<true>'
wait_for "42% discharging: level 42" "s dumpsys battery" "level: 42"
wait_for "42% discharging: on battery" "s dumpsys battery" "AC powered: false" "AC powered"
wait_for "42% discharging: status discharging" "s dumpsys battery" "status: 3" "status"

# Power saver: power-profiles-daemon's profile.
set_prop net.hadess.PowerProfiles /net/hadess/PowerProfiles net.hadess.PowerProfiles ActiveProfile '<"power-saver">'
wait_for "power-saver profile: battery saver on" "s settings get global low_power" "^1$"
set_prop net.hadess.PowerProfiles /net/hadess/PowerProfiles net.hadess.PowerProfiles ActiveProfile '<"balanced">'
wait_for "balanced profile: battery saver off" "s settings get global low_power" "^0$"

# Metered: reaches the system app (host mode can't apply it to Ethernet).
set_prop org.freedesktop.NetworkManager /org/freedesktop/NetworkManager org.freedesktop.NetworkManager Metered '<uint32 1>'
if [[ ${LINUXAPK_MODE:-flatpak} == flatpak ]]; then
  wait_for "host network becomes metered" "s logcat -d -s LinuxApkHostNetwork:I" "host network metered"
else
  wait_for "metered host connection reaches Android" "s logcat -d -s LinuxApkHostState:I" "host network metered=2"
fi

# Locale: plasma-localerc, live. US English starts weeks on Sunday, so
# LC_TIME=en_GB adds the Monday week start as a regional preference.
printf '[Formats]\nLANG=de_DE.UTF-8\n' >"$T/config/plasma-localerc"
wait_for "plasma-localerc LANG: system locale de-DE" "s settings get system system_locales" "^de-DE$"
printf '[Formats]\nLANG=en_US.UTF-8\nLC_TIME=en_GB.UTF-8\n' >"$T/config/plasma-localerc"
wait_for "LC_TIME en_GB: Monday week start" "s settings get system system_locales" "^en-US-u-fw-mon$"

# Location: the probes app (permissions granted) asks network, gps and fused.
size=$(stat -c %s "$probes")
s cmd package install -r -g -S "$size" <"$probes" >/dev/null
s logcat -c
s am start -n dev.linuxapk.probes/.LocationProbe --ei seconds 30 >/dev/null
wait_for "the bridge asks the desktop once an app asks" "s logcat -d -s LinuxApkLocation:I" "asking the desktop"
if [[ -n $deny ]]; then
  wait_for "portal denial: providers off" "s logcat -d -s LinuxApkLocationProbe:I" "provider (network|gps) disabled"
  sleep 2
  call "$DBUS_SESSION_BUS_ADDRESS" org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop \
    dev.linuxapk.Mock.Fix 48.8584 2.2945 25.0 || true
  sleep 3
  if s logcat -d -s LinuxApkLocationProbe:I | grep -q "fix from"; then
    bad "portal denial: the app gets no fix"
  else
    ok "portal denial: the app gets no fix"
  fi
  wait_for "portal denial: Android's permission still granted" \
    "s dumpsys package dev.linuxapk.probes" "ACCESS_FINE_LOCATION: granted=true"
  s am start -n dev.linuxapk.probes/.LocationProbe --ei seconds 5 >/dev/null 2>&1
  sleep 6
  asked=$(grep -c "^start " "$T/portal.log" || true)
  if ((asked == 1)); then ok "portal denial: the desktop was asked once"; else bad "portal denial: the desktop was asked $asked times"; fi
else
  for _ in 1 2 3; do
    sleep 2
    call "$DBUS_SESSION_BUS_ADDRESS" org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop \
      dev.linuxapk.Mock.Fix 48.8584 2.2945 25.0
  done
  for p in network gps fused; do
    wait_for "probe gets a $p fix" "s logcat -d -s LinuxApkLocationProbe:I" "fix from=$p provider=[a-z]+ lat=48.8584 lon=2.2945"
  done
  wait_for "dumpsys location has the fix" "s dumpsys location" "48.8584"
fi
if [[ -n $keep ]]; then
  echo "kept: LINUXAPK_INSTANCE=$LINUXAPK_INSTANCE; Ctrl-C or SIGTERM stops it" >&2
  trap 'exit $fail' INT TERM
  sleep infinity &
  pids+=($!)
  wait
fi
exit $fail
