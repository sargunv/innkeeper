#!/usr/bin/env bash
# Exercise real Android foreground audio, idle stop and boot-rescheduled alarms.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-lifecycle}
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then exec "$repo/tests/nested.sh" "$0" "$@"; fi
export LINUXAPK_IDLE_GRACE=10 LINUXAPK_AUDIO_SINK=linuxapk_lifecycle
S=$repo/tools/dev/session.sh
T=$repo/out/test/lifecycle/$LINUXAPK_INSTANCE
mkdir -p "$T"
mod=$(pactl load-module module-null-sink sink_name=linuxapk_lifecycle rate=48000 channels=2)
# shellcheck disable=SC2329 # invoked by EXIT
cleanup() {
  "$S" stop >/dev/null 2>&1 || true
  "$S" reset >/dev/null 2>&1 || true
  pactl unload-module "$mod"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
"$S" stop >/dev/null
"$S" reset >/dev/null
"$S" start
boot() {
  for _ in $(seq 120); do
    [[ $("$S" exec getprop sys.boot_completed 2>/dev/null) == 1 ]] && return
    sleep 1
  done
  echo 'FAIL boot timeout' >&2
  return 1
}
boot
apk=$repo/out/test/probes/LinuxApkProbes.apk
size=$(stat -c %s "$apk")
"$S" exec cmd package install -r -g -S "$size" <"$apk"
"$S" exec cmd appops set dev.linuxapk.probes SCHEDULE_EXACT_ALARM allow
"$S" exec cmd media_session volume --stream 3 --set 15 >/dev/null
"$S" exec am start -n dev.linuxapk.probes/.LifecycleProbe --es action media
sleep 30
"$S" status
# Capture after three idle grace periods and with the probe activity closed.
timeout 5 parec -d linuxapk_lifecycle.monitor --format=s16le --rate=48000 --channels=2 --raw >"$T/media.raw" || true
/usr/bin/python3 "$repo/tests/e2e/audio/freq.py" "$T/media.raw" --expect 440
"$S" exec logcat -d -s LinuxApkLifecycleProbe:I
printf 'ok   foreground audio survives closed activity and idle grace\n'
"$S" exec am start -n dev.linuxapk.probes/.LifecycleProbe --es action network
for _ in $(seq 30); do
  log=$("$S" exec logcat -d -s LinuxApkLifecycleProbe:I)
  if grep -q 'network status=200 bytes=' <<<"$log"; then break; fi
  sleep 1
done
grep -q 'network status=200 bytes=' <<<"$log" || {
  echo "FAIL Android app HTTPS: $log"
  exit 1
}
echo 'ok   Android app HTTPS request returned 200'

"$S" exec am start -n dev.linuxapk.probes/.LifecycleProbe --es action alarm --ei seconds 240
"$S" exec am start -n dev.linuxapk.probes/.LifecycleProbe --es action stop
wakeup=$("$S" exec logcat -d -s LinuxApkLifecycleProbe:I | sed -n 's/.*alarm scheduled at=\([0-9]*\).*/\1/p' | tail -1)
[[ -n $wakeup ]] || {
  echo "FAIL alarm was not scheduled"
  exit 1
}
# status does not keep the exec lock alive.
for _ in $(seq 60); do
  if status=$("$S" status); then sleep 1; else break; fi
done
[[ $status == stopped* ]] || {
  echo "FAIL idle shutdown: $status"
  exit 1
}
if [[ ${LINUXAPK_MODE:-flatpak} == flatpak ]]; then
  grep -qx "wakeup=$wakeup" <<<"$status" || {
    echo "FAIL alarm waiter: $status"
    exit 1
  }
else
  env XDG_RUNTIME_DIR="/run/user/$UID" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$UID/bus" \
    systemctl --user is-active --quiet "linuxapk-session-$LINUXAPK_INSTANCE-wake.timer"
fi
printf 'ok   idle stopped with pending alarm: %s\n' "$status"
wake_checked=
for _ in $(seq 240); do
  # An autonomous wake is a boot too. Cancel rather than violate the host's
  # memory floor if another workload grew while the waiter was idle.
  if (($(date +%s) >= wakeup / 1000 - 65)); then
    if [[ -z $wake_checked ]]; then
      free -g
      wake_checked=1
    fi
    available=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
    if ((available < 25 * 1024 * 1024)); then
      echo 'DEFER alarm wake: less than 25 GiB available'
      exit 77
    fi
  fi
  if "$S" status >/dev/null; then break; fi
  sleep 1
done
boot
cg=$("$S" cgroup)
[[ $(cat "$cg/memory.max") == 8589934592 ]] || {
  echo 'FAIL replacement sandbox memory cap'
  exit 1
}
"$S" exec cat /system_ext/priv-app/LinuxApkSystem/LinuxApkSystem.apk |
  cmp - "$repo/out/stage/share/linuxapk/android/system_ext/priv-app/LinuxApkSystem/LinuxApkSystem.apk"
printf 'ok   alarm restarts current stage in capped scope\n'
for _ in $(seq 100); do
  log=$("$S" exec logcat -d -s LinuxApkLifecycleProbe:I)
  if grep -q 'alarm fired at=' <<<"$log"; then
    echo "$log"
    echo 'ok   rescheduled Android alarm delivered after wake'
    exit 0
  fi
  sleep 1
done
echo 'FAIL alarm not delivered' >&2
exit 1
