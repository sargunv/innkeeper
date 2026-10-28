#!/usr/bin/env bash
# End-to-end input test against a running session that has reached
# sys.boot_completed=1 (see docs/input.md). Checks that Linux.apk System is
# installed and running and holds the bridge's input service, installs the
# probes APK (out/test/probes), opens its InputProbeActivity in a window of its
# own through the window manager's control command (docs/display.md), then
# injects tests/e2e/system-app/probe-stream.txt (mouse, wheel, keys, two-finger
# touch, touchpad scroll and gestures, pen and eraser) through the system app's
# translator and injector (the probes' InputStdinHarness), as the input
# service's lines are, and checks what the activity received. The stream's DISPLAY placeholders become the probe's
# display id, so every event must arrive there.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
s() { "$session" exec "$@"; }
probes=$repo/out/test/probes/LinuxApkProbes.apk
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
[[ -f $probes ]] || {
  echo "no $probes (run \`mise run build\`)" >&2
  exit 2
}

pkg=$(s dumpsys package dev.linuxapk.system)
check "installed as a privileged system app" "codePath=/system_ext/priv-app/LinuxApkSystem" "$pkg"
check "INJECT_EVENTS granted" "android.permission.INJECT_EVENTS: granted=true" "$pkg"
if s pidof dev.linuxapk.system >/dev/null; then ok "persistent process running"; else bad "persistent process running"; fi
check "service started" "started input" "$(s logcat -d -s LinuxApkSystem:I)"
# The input service (id 6) admits only the system app's uid.
check "connected to the bridge's input service" "connected to bridge service 6" \
  "$(s logcat -d -s LinuxApkBridge:I)"
# Text input (docs/input.md): the Linux.apk Keyboard is the input method, so no soft keyboard.
check "Linux.apk Keyboard is the default input method" "^dev.linuxapk.system/.ime.HostImeService" \
  "$(s settings get secure default_input_method)"

wm() { s am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a "dev.linuxapk.$1" "${@:2}"; }
for p in ADD_TRUSTED_DISPLAY ADD_ALWAYS_UNLOCKED_DISPLAY INTERNAL_SYSTEM_WINDOW MANAGE_ACTIVITY_TASKS REAL_GET_TASKS; do
  check "$p granted" "android.permission.$p: granted=true" "$pkg"
done
check "window manager answers" "result=-1" "$(wm LIST)"
size=$(stat -c %s "$probes")
check "probes installed" "^Success" "$(s cmd package install -r -g -S "$size" <"$probes")"
harness=$(s pm path dev.linuxapk.probes | sed -n 's/^package://p' | tr -d '\r')
wm CLOSE --es package dev.linuxapk.probes >/dev/null || true
s logcat -c
out=$(wm LAUNCH --es package dev.linuxapk.probes --es component dev.linuxapk.probes/.InputProbeActivity)
display=$(sed -n 's/.*launched display=\([0-9]*\).*/\1/p' <<<"$out")
if [[ -z $display ]]; then
  bad "launch in a window: $out"
  exit 1
fi
ok "launch in a window"
sleep 2
check "listed as a window" "display=$display package=dev.linuxapk.probes" "$(wm LIST)"
sleep 1
sed "s/DISPLAY/$display/" "$here/probe-stream.txt" |
  s sh -c "CLASSPATH=$harness app_process / dev.linuxapk.probes.InputStdinHarness" >/dev/null
sleep 1
log=$(s logcat -d -s LinuxApkProbe:I InputDispatcher:E)
check "mouse hover" "generic .*ACTION_HOVER_MOVE.*source=0x2002" "$log"
check "mouse click" "touch .*ACTION_DOWN.*buttonState=BUTTON_PRIMARY.*source=0x2002" "$log"
check "mouse button press" "touch .*ACTION_BUTTON_PRESS|generic .*ACTION_BUTTON_PRESS" "$log"
check "mouse wheel" "generic .*ACTION_SCROLL" "$log"
check "shift+a" "key .*keyCode=KEYCODE_A.*META_SHIFT_ON" "$log"
check "escape" "key .*keyCode=KEYCODE_ESCAPE" "$log"
check "two-finger touch" "touch .*ACTION_POINTER_DOWN.*source=0x1002" "$log"
# Touchpad scrolling and gestures are fake fingers from the mouse, with a classification.
check "touchpad scroll: fake finger down" \
  "touch .*ACTION_DOWN.*TOOL_TYPE_FINGER.*TWO_FINGER_SWIPE.*source=0x2002" "$log"
check "touchpad scroll: moves with the scroll distance" "touch .*ACTION_MOVE.*TWO_FINGER_SWIPE.*scrollY" "$log"
check "touchpad scroll: ends in UP" "touch .*ACTION_UP.*TWO_FINGER_SWIPE" "$log"
check "a hold stops the fling" "touch .*ACTION_CANCEL.*TWO_FINGER_SWIPE" "$log"
check "pinch: second finger" "touch .*ACTION_POINTER_DOWN.*classification=PINCH.*source=0x2002" "$log"
check "pinch: scale factor" "touch .*ACTION_MOVE.*classification=PINCH.*pinch\[0\]=1\.5" "$log"
check "pen hovers" "generic .*ACTION_HOVER_MOVE.*TOOL_TYPE_STYLUS.*source=0x5002.*distance\[0\]" "$log"
check "pen pressure" "touch .*ACTION_MOVE.*TOOL_TYPE_STYLUS.*pressure\[0\]=0\.76" "$log"
check "eraser" "touch .*ACTION_DOWN.*TOOL_TYPE_ERASER.*source=0x5002" "$log"
if grep -q "Inconsistent event" <<<"$log"; then bad "dispatcher accepted every event"; else ok "dispatcher accepted every event"; fi
check "events carry the window's display" "displayId=$display" "$log"
if grep -E "LinuxApkProbe.*displayId=" <<<"$log" | grep -vq "displayId=$display"; then
  bad "no event on another display"
else
  ok "no event on another display"
fi
out=$(wm CLOSE --ei display "$display")
check "close the window" "closed display=$display" "$out"
sleep 1
if wm LIST | grep -q "display=$display "; then bad "window gone after close"; else ok "window gone after close"; fi
exit $fail
