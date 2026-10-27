#!/usr/bin/env bash
# End-to-end input test against a running session that has reached
# sys.boot_completed=1 (see docs/input.md). Checks that Linux.apk System is
# installed and running, then injects android/e2e/probe-stream.txt (mouse,
# wheel, keys, two-finger touch) through the app's own translator/injector into
# InputProbeActivity and checks what the activity received.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
session=${LINUXAPK_SESSION:-$repo/session/linuxapk-session}
s() { "$session" exec "$@"; }
apk=/system_ext/priv-app/LinuxApkSystem/LinuxApkSystem.apk
fail=0
ok() { echo "ok   $1"; }
bad() {
  echo "FAIL $1"
  fail=1
}
check() { if grep -qE "$2" <<<"$3"; then ok "$1"; else bad "$1"; fi; }

[[ $(s getprop sys.boot_completed | tr -d '\r') == 1 ]] || {
  echo "session has not booted (sys.boot_completed != 1)" >&2
  exit 2
}

pkg=$(s dumpsys package dev.linuxapk.system)
check "installed as a privileged system app" "codePath=/system_ext/priv-app/LinuxApkSystem" "$pkg"
check "INJECT_EVENTS granted" "android.permission.INJECT_EVENTS: granted=true" "$pkg"
check "HOST_INPUT granted" "dev.linuxapk.permission.HOST_INPUT: granted=true" "$pkg"
if pid=$(s pidof dev.linuxapk.system); then
  ok "persistent process running"
  # HOST_INPUT maps to gid input (1004), for a 0660 system:input socket.
  check "holds gid input" "^Groups:.*\b1004\b" "$(s cat "/proc/${pid//[^0-9]/}/status")"
else
  bad "persistent process running"
fi
check "service started" "started input" "$(s logcat -d -s LinuxApkSystem:I)"
input_log=$(s logcat -d -s LinuxApkInput:I)
if grep -q "connected to" <<<"$input_log"; then
  ok "connected to the composer's input socket"
else
  echo "note: not connected to the input socket (headless composer?):"
  tail -n 2 <<<"$input_log"
fi

s logcat -c
s am start -W -n dev.linuxapk.system/.input.InputProbeActivity >/dev/null
sleep 1
s sh -c "CLASSPATH=$apk app_process / dev.linuxapk.system.input.InputBridgeMain --stdin" \
  <"$here/probe-stream.txt" >/dev/null
sleep 1
log=$(s logcat -d -s LinuxApkProbe:I InputDispatcher:E)
check "mouse hover" "generic .*ACTION_HOVER_MOVE.*source=0x2002" "$log"
check "mouse click" "touch .*ACTION_DOWN.*buttonState=BUTTON_PRIMARY.*source=0x2002" "$log"
check "mouse button press" "touch .*ACTION_BUTTON_PRESS|generic .*ACTION_BUTTON_PRESS" "$log"
check "mouse wheel" "generic .*ACTION_SCROLL" "$log"
check "shift+a" "key .*keyCode=KEYCODE_A.*META_SHIFT_ON" "$log"
check "escape" "key .*keyCode=KEYCODE_ESCAPE" "$log"
check "two-finger touch" "touch .*ACTION_POINTER_DOWN.*source=0x1002" "$log"
if grep -q "Inconsistent event" <<<"$log"; then bad "dispatcher accepted every event"; else ok "dispatcher accepted every event"; fi
exit $fail
