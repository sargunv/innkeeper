#!/usr/bin/env bash
# One runner, one private desktop/session, independently selectable bounded groups.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-e2e}
export LINUXAPK_E2E=1
S=$repo/tools/dev/session.sh
T=$repo/out/test/e2e/$LINUXAPK_INSTANCE
desktop_log=$repo/out/test/nested/$LINUXAPK_INSTANCE/desktop.log
bridge_log=$repo/out/test/nested/$LINUXAPK_INSTANCE/state/linuxapk-$LINUXAPK_INSTANCE/bridge.log
apk=$repo/out/test/probes/LinuxApkProbes.apk
s() { "$S" exec "$@"; }
wm() { s am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a "dev.linuxapk.$1" "${@:2}"; }
# Poll observations, returning as soon as they arrive; the outer group deadline
# also bounds a command that stops answering.
until_ok() {
  local end=$((SECONDS + 20))
  until "$@"; do
    ((SECONDS < end)) || {
      echo "FAIL waiting for $*" >&2
      return 1
    }
    sleep 0.1
  done
}
booted() { [[ $(s getprop sys.boot_completed 2>/dev/null) == 1 ]]; }
log_has() { s logcat -d -s "$1:I" | grep -E "$2" >/dev/null; }
file_has() { grep -E "$2" "$1" >/dev/null; }
window_ready() { s dumpsys activity activities | grep "topResumedActivity=.*$1" >/dev/null; }
launch() {
  local out
  wm CLOSE --es package dev.linuxapk.probes >/dev/null
  out=$(wm LAUNCH --es package dev.linuxapk.probes --es component "dev.linuxapk.probes/.$1")
  display=$(sed -n 's/.*launched display=\([0-9]*\).*/\1/p' <<<"$out")
  [[ -n $display ]] || {
    echo "FAIL launch: $out"
    return 1
  }
  if [[ $1 != DesktopProbe ]]; then until_ok window_ready "$1"; fi
}
stopped() { [[ $("$S" status) == stopped* ]]; }
running() { "$S" status >/dev/null; }
stop_session() {
  "$S" stop >/dev/null
  # The dev supervisor publishes its exit after the sandbox closes. Wait for
  # it before a restart, so its old completion cannot fail the new start.
  if [[ -f $repo/out/dev/flatpak/$LINUXAPK_INSTANCE/cgroup ]]; then
    until_ok test -f "$repo/out/dev/flatpak/$LINUXAPK_INSTANCE/ended"
  fi
}

if [[ ${1:-} == --group ]]; then
  case $2 in
    boot)
      "$S" selftest
      s getprop ro.product.board >"$T/board.txt"
      [[ $(cat "$T/board.txt") == cutf ]]
      # Cuttlefish's netd aborts during startup if mapLockTest fails. Its
      # published binder service proves startup passed those OFD checks.
      s service check netd >"$T/netd.txt"
      file_has "$T/netd.txt" '^Service netd: found$'
      s pidof netd >>"$T/netd.txt"
      echo 'ok   image board=cutf; netd published after mapLockTest'
      s timeout 10 logcat -b main,system,crash -d -t 10000 -s cutils-trace:E >"$T/trace.txt"
      if file_has "$T/trace.txt" 'Error opening trace file'; then
        echo 'FAIL expected missing-tracefs error was not silenced' >&2
        exit 1
      fi
      echo 'ok   no missing-tracefs noise'
      s cmd package resolve-activity --brief -a android.intent.action.MAIN \
        -c android.intent.category.HOME >"$T/home.txt"
      file_has "$T/home.txt" '^dev.linuxapk.system/.wm.HomeActivity$'
      [[ -z $(s pm path com.android.launcher3) ]]
      sysui=$(s pidof com.android.systemui)
      s input -d 0 keyevent KEYCODE_APP_SWITCH KEYCODE_HOME KEYCODE_BACK
      until_ok window_ready HomeActivity
      [[ $(s pidof com.android.systemui) == "$sysui" ]]
      s dumpsys activity service com.android.systemui/.SystemUIService LauncherProxyService >"$T/overview.txt"
      file_has "$T/overview.txt" 'mIsEnabled=false'
      file_has "$T/overview.txt" 'mBound=false'
      echo 'ok   HOME resolves without Launcher3; recents/Home/Back leave SystemUI and HOME running'
      ;;
    storage)
      s cmd appops set dev.linuxapk.probes MANAGE_EXTERNAL_STORAGE allow
      s am start -n dev.linuxapk.probes/.StorageProbe --es round initial >/dev/null
      until_ok log_has LinuxApkStorageProbe 'state=mounted app-dir=ok MediaStore=ok round=initial'
      provider=$(s pidof com.android.providers.media.module)
      [[ -n $provider ]]
      # shellcheck disable=SC2329 # invoked through until_ok
      volume_state() { s sm list-volumes all | grep -q "^emulated;0 $1 "; }
      s sm unmount 'emulated;0'
      until_ok volume_state unmounted
      [[ $(s pidof com.android.providers.media.module) == "$provider" ]]
      s sm mount 'emulated;0'
      until_ok volume_state mounted
      [[ $(s pidof com.android.providers.media.module) == "$provider" ]]
      s am start -n dev.linuxapk.probes/.StorageProbe --es round remounted >/dev/null
      until_ok log_has LinuxApkStorageProbe 'state=mounted app-dir=ok MediaStore=ok round=remounted'
      ;;
    input)
      launch InputProbeActivity
      # shellcheck disable=SC2329 # invoked through until_ok
      mapped() { wm LIST | grep "display=$display package=dev.linuxapk.probes .*window=yes" >/dev/null; }
      until_ok mapped
      s dumpsys input >"$T/devices.txt"
      file_has "$T/devices.txt" 'Linux.apk Keyboard'
      file_has "$T/devices.txt" 'Linux.apk Mouse'
      # Keep one fake KWin device across enter/leave and drag. Submit the
      # ordered stream without observation barriers between evdev devices.
      mkfifo "$T/seat"
      /usr/bin/python3 "$here/seat.py" --stdin <"$T/seat" &
      seat_pid=$!
      exec {seat_fd}>"$T/seat"
      printf '%s\n' key:30,1 key:30,0 \
        abs:900,300 abs:100,300 abs:900,300 button:272,1 \
        key:42,1 abs:1000,320 key:42,0 abs:1100,340 button:272,0 >&"$seat_fd"
      # One bounded capture checks the delivered sequence, avoiding a separate
      # exec/logcat round trip for every intermediate observation.
      # shellcheck disable=SC2329 # invoked through until_ok
      native_input_ready() {
        s logcat -d -s LinuxApkProbe:I >"$T/modifier-drag.txt"
        file_has "$T/modifier-drag.txt" 'key .*KEYCODE_A.*deviceId=-1' &&
          file_has "$T/modifier-drag.txt" 'touch .*ACTION_UP.*metaState=0.*deviceName=Linux.apk Mouse'
      }
      until_ok native_input_ready
      for pattern in 'configuration keyboard=2' \
        'generic .*ACTION_HOVER_(ENTER|MOVE).*deviceName=Linux.apk Mouse' \
        'generic .*ACTION_HOVER_EXIT.*deviceName=Linux.apk Mouse' \
        'touch .*ACTION_DOWN.*deviceName=Linux.apk Mouse' \
        'key .*ACTION_DOWN.*KEYCODE_SHIFT_LEFT.*deviceName=Linux.apk Keyboard' \
        'touch .*ACTION_MOVE.*META_SHIFT_ON.*deviceName=Linux.apk Mouse' \
        'key .*ACTION_UP.*KEYCODE_SHIFT_LEFT.*deviceName=Linux.apk Keyboard'; do
        file_has "$T/modifier-drag.txt" "$pattern"
      done
      [[ $(grep -Ec 'key .*KEYCODE_SHIFT_LEFT' "$T/modifier-drag.txt") == 2 ]]
      if file_has "$T/modifier-drag.txt" '(touch|generic) .*deviceId=-1'; then
        echo 'FAIL physical mouse switched to injection' >&2
        exit 1
      fi
      echo 'ok   virtual keyboard and mouse enumerated; QWERTY configuration; ordered seat key injection (Android device ID -1)'
      # Put a second probe on a separately owned window/display. A second task
      # on Settings' display keeps the production one-window-per-package policy.
      second=$(wm LAUNCH --es package com.android.settings --es component com.android.settings/.Settings)
      second_display=$(sed -n 's/.*launched display=\([0-9]*\).*/\1/p' <<<"$second")
      [[ -n $second_display && $second_display != "$display" ]]
      s am start --display "$second_display" -f 0x18000000 -n dev.linuxapk.probes/.InputProbeActivity >/dev/null
      until_ok log_has LinuxApkProbe "configuration keyboard=2 windowDisplay=$second_display"
      cat >"$T/two-windows.js" <<'JS'
let x = 0;
for (const w of workspace.windowList()) {
  if (String(w.resourceClass).includes("dev.linuxapk.")) {
    w.minimized = false;
    w.frameGeometry = {x: x, y: 100, width: 800, height: 800};
    x += 1000;
  }
}
JS
      script_id=$(gdbus call --session --dest org.kde.KWin --object-path /Scripting \
        --method org.kde.kwin.Scripting.loadScript "$T/two-windows.js" linuxapk-input-two | grep -oE '[0-9]+')
      gdbus call --session --dest org.kde.KWin --object-path "/Scripting/Script$script_id" \
        --method org.kde.kwin.Script.run >/dev/null
      gdbus call --session --dest org.kde.KWin --object-path /Scripting \
        --method org.kde.kwin.Scripting.unloadScript linuxapk-input-two >/dev/null
      # Rapid alternating enter/move/click/leave reports must retain their own
      # display's device even with both endpoints readable.
      for _ in {1..2}; do
        printf 'abs:200,300\nbutton:272,1\nbutton:272,0\nabs:1200,300\nbutton:272,1\nbutton:272,0\n' >&"$seat_fd"
      done
      until_ok log_has LinuxApkProbe "touch .*ACTION_UP.*deviceName=Linux.apk Mouse $display windowDisplay=$display"
      until_ok log_has LinuxApkProbe "touch .*ACTION_UP.*deviceName=Linux.apk Mouse $second_display windowDisplay=$second_display"
      s logcat -d -s LinuxApkProbe:I >"$T/two-windows.txt"
      /usr/bin/python3 - "$T/two-windows.txt" <<'PYTEST'
import re, sys
for device, window in re.findall(r'deviceName=Linux.apk Mouse (\d+) windowDisplay=(\d+)', open(sys.argv[1]).read()):
  assert device == window, (device, window)
PYTEST
      s dumpsys input >"$T/two-devices.txt"
      file_has "$T/two-devices.txt" "Linux.apk Mouse $display"
      file_has "$T/two-devices.txt" "Linux.apk Mouse $second_display"
      wm CLOSE --ei display "$second_display" >/dev/null
      # Removal must leave the first display's mouse alive.
      # shellcheck disable=SC2329 # invoked through until_ok
      mouse_removed() {
        s dumpsys input >"$T/remaining-device.txt"
        ! file_has "$T/remaining-device.txt" "Linux.apk Mouse $second_display"
      }
      until_ok mouse_removed
      file_has "$T/remaining-device.txt" "Linux.apk Mouse $display"
      echo 'ok   two windows retain distinct mouse devices across rapid crossings; removed with their display'
      exec {seat_fd}>&-
      wait "$seat_pid"
      rm "$T/seat"
      echo 'ok   native mouse hover exit and modifier changes during drag; no duplicate modifier keys'
      harness=$(s pm path dev.linuxapk.probes | sed -n 's/^package://p' | tr -d '\r')
      sed "s/DISPLAY/$display/" "$here/system-app/probe-stream.txt" |
        s sh -c "CLASSPATH=$harness app_process / dev.linuxapk.probes.InputStdinHarness" >"$T/input.txt"
      until_ok log_has LinuxApkProbe 'key .*KEYCODE_A.*META_SHIFT_ON'
      until_ok log_has LinuxApkProbe 'touch .*ACTION_DOWN.*source=0x2002'
      until_ok log_has LinuxApkProbe 'generic .*ACTION_SCROLL'
      wm CLOSE --ei display "$display" | grep -q "closed display=$display"
      launch WindowModeProbe
      # A manifest-locked portrait activity starts portrait; a public runtime request reshapes
      # the actual host window, whose configure comes back through the bridge as this size.
      # shellcheck disable=SC2329 # invoked through until_ok
      shape() {
        wm LIST | /usr/bin/python3 -c '
import re, sys
for line in sys.stdin:
    m = re.search(r"display=" + sys.argv[1] + r" .*size=(\d+)x(\d+)", line)
    if m and ((int(m[1]) < int(m[2])) == (sys.argv[2] == "portrait")):
        sys.exit(0)
sys.exit(1)
' "$display" "$1"
      }
      until_ok shape portrait
      s am start --display "$display" -n dev.linuxapk.probes/.WindowModeProbe --es action landscape >/dev/null
      until_ok shape landscape
      wm CLOSE --ei display "$display" >/dev/null
      echo 'ok   requested-orientation host window shape (portrait -> landscape)'
      /usr/bin/python3 "$here/app-settings.py"
      ;;
    audio)
      s cmd media_session volume --stream 3 --set 15 >/dev/null
      parec -d "$LINUXAPK_AUDIO_SINK.monitor" --format=s16le --rate=48000 --channels=2 --raw >"$T/audio.raw" &
      recorder=$!
      trap 'kill "$recorder" 2>/dev/null || true; wait "$recorder" 2>/dev/null || true' EXIT
      s am start-foreground-service -n "dev.linuxapk.probes/.LifecycleProbe\$Media" >/dev/null
      until_ok log_has LinuxApkLifecycleProbe 'foreground audio started'
      # FFT succeeds only after the capture has enough non-silent tone samples.
      until_ok /usr/bin/python3 "$here/audio/freq.py" "$T/audio.raw" --expect 440
      # Android am returns 1 when a service was successfully stopped.
      s am stopservice -n "dev.linuxapk.probes/.LifecycleProbe\$Media" >/dev/null || true
      ;;
    desktop)
      # One live preference round trip, through the same Session1 API as GTK.
      dest=dev.linuxapk.Manager.Session.i_${LINUXAPK_INSTANCE//-/_}
      gdbus call --session --dest "$dest" --object-path /dev/linuxapk/Session \
        --method dev.linuxapk.Session1.SetPreferences '{"follow_accent":false}' >/dev/null
      gdbus call --session --dest "$dest" --object-path /dev/linuxapk/Session \
        --method dev.linuxapk.Session1.GetPreferences >"$T/preferences.txt"
      file_has "$T/preferences.txt" '"follow_accent":false'
      until_ok log_has LinuxApkSettings 'desktop settings: .*followAccent=false'
      gdbus call --session --dest "$dest" --object-path /dev/linuxapk/Session \
        --method dev.linuxapk.Session1.SetPreferences '{}' >/dev/null
      until_ok log_has LinuxApkSettings 'desktop settings: .*followAccent=true'
      echo 'ok   preference persisted and applied live over Session1'
      s am start -n dev.linuxapk.probes/.DesktopProbe --es action link >/dev/null
      until_ok file_has "$bridge_log" 'openuri: .*https://example.com/linuxapk-e2e'
      s am start -a android.intent.action.VIEW -d mailto:e2e@example.com >/dev/null
      until_ok file_has "$bridge_log" 'handoff: .*mailto:e2e@example.com'
      s am start -n dev.linuxapk.probes/.HandoffProbe --es action view >/dev/null
      until_ok file_has "$bridge_log" 'handoff: .*action 2, 1 files'
      s pm enable "dev.linuxapk.probes/.HandoffProbe\$Claimed" >/dev/null
      s am start -a android.intent.action.VIEW -d mailto:android-handler@example.com >/dev/null
      until_ok log_has LinuxApkHandoffProbe 'Android handler: mailto:android-handler@example.com'
      s am start -n dev.linuxapk.probes/.HandoffProbe --es action view >/dev/null
      until_ok log_has LinuxApkHandoffProbe 'Android handler: content://dev.linuxapk.probes.handoff/shared.odt'
      s pm disable "dev.linuxapk.probes/.HandoffProbe\$Claimed" >/dev/null
      s am start -n dev.linuxapk.probes/.NotificationProbe >/dev/null
      until_ok file_has "$bridge_log" 'notify: .*not shown \(test session\)'
      for mode in progress style indeterminate style-indeterminate ongoing no-badge; do
        expected=42 badge=true
        case $mode in
          *indeterminate) expected=-2 ;;
          ongoing | no-badge) expected=-1 badge=false ;;
        esac
        s am start -n dev.linuxapk.probes/.NotificationProbe --es mode "$mode" >/dev/null
        until_ok file_has "$bridge_log" "not shown .*Ada Lovelace $mode.*progress=$expected badge=$badge"
      done
      s am start -n dev.linuxapk.probes/.NotificationProbe --es mode cancel >/dev/null
      printf %s linuxapk-e2e-host-before | wl-copy
      s am start -n dev.linuxapk.probes/.ClipProbe --es text linuxapk-e2e-android >/dev/null
      until_ok log_has LinuxApkClipProbe 'set: linuxapk-e2e-android'
      [[ $(timeout 2 wl-paste --no-newline) == linuxapk-e2e-host-before ]]
      printf %s linuxapk-e2e-host-after | wl-copy
      s am start -n dev.linuxapk.probes/.ClipProbe >/dev/null
      until_ok log_has LinuxApkClipProbe 'clip: .*text=linuxapk-e2e-android'
      # A process cold start exercises PickerActivity's connection callback.
      s kill -9 "$(s pidof dev.linuxapk.system)"
      launch DesktopProbe
      until_ok file_has "$bridge_log" 'documents: .*pick cancelled \(test session\)'
      until_ok log_has LinuxApkDocs 'pick ended: -125'
      until_ok log_has LinuxApkDesktopProbe 'picked: null'
      if file_has "$desktop_log" 'picker:|notification:'; then
        echo 'FAIL unexpected desktop picker or notification' >&2
        exit 1
      fi
      echo 'ok   clipboard isolated both ways; cold PICK cancelled without a portal'
      # Hide only this instance's socket to exercise the asynchronous connection timeout.
      socket=$XDG_RUNTIME_DIR/app/dev.linuxapk.Manager/linuxapk-$LINUXAPK_INSTANCE/bridge/bridge.sock
      mv "$socket" "$socket.hidden"
      trap 'mv "$socket.hidden" "$socket" 2>/dev/null || true' EXIT
      s kill -9 "$(s pidof dev.linuxapk.system)"
      s am start -n dev.linuxapk.system/.documents.PickerActivity \
        -a android.intent.action.OPEN_DOCUMENT -t text/plain >/dev/null
      until_ok log_has LinuxApkDocs 'no desktop file chooser; handing .* to DocumentsUI'
      # shellcheck disable=SC2329 # invoked through until_ok
      documents_ui() { s dumpsys activity activities | grep 'topResumedActivity=.*com.android.documentsui' >/dev/null; }
      until_ok documents_ui
      mv "$socket.hidden" "$socket"
      trap - EXIT
      s am force-stop com.android.documentsui
      echo 'ok   unavailable bridge falls back to DocumentsUI after the connection timeout'
      wm CLOSE --es package dev.linuxapk.probes >/dev/null
      printf '# linuxapk-file-grant\n' >"$T/file.md"
      "$repo/tools/dev/linuxapk.sh" run dev.linuxapk.probes "$T/file.md"
      until_ok log_has LinuxApkFileProbe 'uri=content://dev.linuxapk.system.documents/document/.*type=text/markdown read=true content=# linuxapk-file-grant'
      until_ok window_ready FileProbe
      s logcat -d -s LinuxApkFileProbe:I >"$T/file-view.txt"
      s dumpsys activity top >"$T/file-activity.txt"
      s env CLASSPATH=/system_ext/priv-app/LinuxApkSystem/LinuxApkSystem.apk \
        app_process / dev.linuxapk.system.apps.IconDump dev.linuxapk.probes >"$T/file-metadata.txt"
      file_has "$T/file-metadata.txt" '^mime text/markdown$'
      file_has "$T/file-metadata.txt" '^mime text/plain$'
      wm CLOSE --es package dev.linuxapk.probes >/dev/null
      printf '# linuxapk-open-with\n' >"$T/file.md"
      "$repo/tools/dev/linuxapk.sh" open "$T/file.md"
      until_ok log_has LinuxApkOpenWith 'chooser type=text/markdown apps=dev.linuxapk.probes'
      until_ok window_ready OpenWithActivity
      chooser=$(wm LIST | sed -n 's/.*display=\([0-9]*\) package=dev.linuxapk.system .*/\1/p')
      s input -d "$chooser" keyevent KEYCODE_DPAD_DOWN KEYCODE_ENTER
      until_ok log_has LinuxApkOpenWith 'selected dev.linuxapk.probes:'
      until_ok log_has LinuxApkFileProbe 'uri=content://dev.linuxapk.system.documents/document/.*type=text/markdown read=true content=# linuxapk-open-with'
      until_ok window_ready FileProbe
      s logcat -d -s LinuxApkOpenWith:I LinuxApkFileProbe:I >"$T/open-with.txt"
      wm CLOSE --es package dev.linuxapk.probes >/dev/null
      printf 'no handler\n' >"$T/file.linuxapk-unknown"
      "$repo/tools/dev/linuxapk.sh" open "$T/file.linuxapk-unknown"
      until_ok log_has LinuxApkOpenWith 'chooser type=application/octet-stream apps=$'
      until_ok window_ready OpenWithActivity
      wm CLOSE --es package dev.linuxapk.system >/dev/null
      echo 'ok   Open With matches VIEW filters, opens the selected app and shows no-match UI'
      ;;
    sharing) "$here/share.sh" ;;
    a11y)
      dbus-send --session --dest=org.a11y.Bus --type=method_call --print-reply /org/a11y/bus \
        org.freedesktop.DBus.Properties.Set string:org.a11y.Status string:IsEnabled variant:boolean:true >/dev/null
      launch A11yProbeActivity
      until_ok file_has "$bridge_log" 'a11y: test session: never active'
      wm CLOSE --es package dev.linuxapk.probes >/dev/null
      ;;
    production)
      # shellcheck source=tests/e2e/production.sh
      source "$here/production.sh"
      ;;
    lifecycle)
      s cmd appops set dev.linuxapk.probes SCHEDULE_EXACT_ALARM allow
      # Allow four seconds to observe idle stop before the six-second wake lead.
      s am start -n dev.linuxapk.probes/.LifecycleProbe --es action alarm --ei seconds 10 >/dev/null
      until_ok log_has LinuxApkLifecycleProbe 'alarm scheduled at='
      wm CLOSE --es package dev.linuxapk.probes >/dev/null
      s touch /data/local/tmp/e2e-idle
      until_ok stopped
      echo 'ok   session stopped idle with an alarm pending'
      until_ok running
      until_ok booted
      ;;
  esac
  echo "ok   $2"
  exit
fi

groups=("$@")
((${#groups[@]})) || groups=(boot storage input audio desktop sharing a11y production lifecycle)
for group in "${groups[@]}"; do
  case $group in boot | storage | input | audio | desktop | sharing | a11y | production | lifecycle) ;; *)
    echo "unknown e2e group: $group" >&2
    exit 2
    ;;
  esac
done
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then
  start=$(date +%s%N)
  result=0
  timeout --kill-after=4 55 "$repo/tests/nested.sh" "$0" "${groups[@]}" &
  runner=$!
  trap 'kill -TERM "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true' INT TERM
  wait "$runner" || result=$?
  trap - INT TERM
  end=$(date +%s%N)
  /usr/bin/python3 -c 'import sys; print(f"e2e: TOTAL wall={(int(sys.argv[2])-int(sys.argv[1]))/1e9:.2f}s")' "$start" "$end"
  exit "$result"
fi
mkdir -p "$T"
export LINUXAPK_IDLE_GRACE=1 LINUXAPK_TEST_TIMERS=1 LINUXAPK_AUDIO_SINK=linuxapk_$LINUXAPK_INSTANCE
keeper=""
mod=$(pactl load-module module-null-sink sink_name="$LINUXAPK_AUDIO_SINK" rate=48000 channels=2)
cleanup() {
  local result=$?
  if ((result != 0)); then
    echo "e2e: Android failure diagnostics: $T/{crash,system}.txt" >&2
    timeout 1 "$S" exec logcat -d -b crash >"$T/crash.txt" 2>&1 || true
    if file_has "$T/crash.txt" 'FATAL EXCEPTION IN SYSTEM PROCESS'; then
      echo 'e2e: system_server crashed (not a boot readiness timeout):' >&2
      cat "$T/crash.txt" >&2
    fi
    timeout 1 "$S" exec logcat -d -b system -t 200 >"$T/system.txt" 2>&1 || true
  fi
  "$S" stop >/dev/null 2>&1 || true
  "$S" reset >/dev/null 2>&1 || true
  if [[ -n $keeper ]]; then wait "$keeper" 2>/dev/null || true; fi
  pactl unload-module "$mod"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
stop_session
"$S" reset >/dev/null
mkdir -p "$(dirname "$bridge_log")"
: >"$bridge_log"
until_ok file_has "$desktop_log" ready
"$S" start >/dev/null
# Hold exec's existing busy lock while the other groups run. Lifecycle releases
# it after scheduling its alarm, so no extra boot is needed to change timers.
until_ok booted
: >"$T/keeper.log"
s sh -c 'echo ready; while ! test -f /data/local/tmp/e2e-idle; do sleep 0.1; done' >"$T/keeper.log" &
keeper=$!
until_ok file_has "$T/keeper.log" '^ready$'
size=$(stat -c %s "$apk")
s cmd package install -r -g -S "$size" <"$apk" >/dev/null
failed=()
run_group() {
  local start=$SECONDS result=0
  echo "=== e2e: $1"
  timeout --kill-after=2 50 "$0" --group "$1" || result=$?
  echo "e2e: $1 elapsed=$((SECONDS - start))s"
  if ((result != 0)); then
    # Preserve the failed activity sequence before lifecycle restarts logd.
    timeout 1 "$S" exec logcat -d -t 1000 -s ActivityTaskManager:I ActivityManager:I LinuxApkHandoff:V >"$T/$1-activities.txt" 2>&1 || true
  fi
  return "$result"
}
# Audio uses a foreground service and a private sink; it can run alongside
# the sequential activity/window checks. Production runs after isolation
# assertions (its fake-service logs are positive), then lifecycle changes state.
audio_pid=""
for group in "${groups[@]}"; do
  if [[ $group == audio ]]; then
    run_group audio &
    audio_pid=$!
  fi
done
for group in "${groups[@]}"; do
  case $group in audio | production | lifecycle) continue ;; esac
  run_group "$group" || failed+=("$group")
done
if [[ -n $audio_pid ]]; then wait "$audio_pid" || failed+=(audio); fi
for group in "${groups[@]}"; do
  if [[ $group == production ]]; then run_group production || failed+=(production); fi
done
for group in "${groups[@]}"; do
  if [[ $group == lifecycle ]]; then run_group lifecycle || failed+=(lifecycle); fi
done
((${#failed[@]} == 0)) || {
  echo "e2e: FAIL (${failed[*]})"
  exit 1
}
echo 'e2e: PASS'
