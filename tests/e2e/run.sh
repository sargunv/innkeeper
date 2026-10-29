#!/usr/bin/env bash
# One runner, one private desktop/session, independently selectable bounded groups.
set -Eeuo pipefail
# Expected empty command substitutions need not fail their parent expression.
# shellcheck disable=SC2329 # invoked by the ERR trap
report_error() {
  if ((BASH_SUBSHELL == 0)); then
    echo "e2e: command failed (status=$1) at line $2: $3" >&2
  fi
}
trap 'report_error "$?" "$LINENO" "$BASH_COMMAND"' ERR
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-e2e}
export LINUXAPK_E2E=1
S=$repo/tools/dev/session.sh
T=$repo/out/test/e2e/$LINUXAPK_INSTANCE
desktop_log=$repo/out/test/nested/$LINUXAPK_INSTANCE/desktop.log
bridge_log=$repo/out/test/nested/$LINUXAPK_INSTANCE/state/linuxapk-$LINUXAPK_INSTANCE/bridge.log
apk=$repo/out/test/probes/LinuxApkProbes.apk
# GitHub's software-rendered desktop is slower than local development.
deadline=55
if [[ ${GITHUB_ACTIONS:-} == true ]]; then
  deadline=${LINUXAPK_E2E_DEADLINE:-55}
fi
observation_wait=$(((20 * deadline + 54) / 55))
timed() (
  local name=$1 start
  shift
  start=$(date +%s%N)
  # shellcheck disable=SC2329 # invoked by the EXIT trap
  finish() {
    local result=$? elapsed
    elapsed=$(($(date +%s%N) - start))
    printf 'e2e: %s elapsed=%d.%03ds status=%d\n' "$name" \
      "$((elapsed / 1000000000))" "$(((elapsed / 1000000) % 1000))" "$result" |
      tee -a "$T/timings.log"
  }
  trap finish EXIT
  trap 'exit 130' INT TERM
  printf '=== e2e: %s\n' "$name" | tee -a "$T/timings.log"
  "$@"
)
s() { "$S" exec "$@"; }
wm() { s am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a "dev.linuxapk.$1" "${@:2}"; }
# Poll observations, returning as soon as they arrive; the outer group deadline
# also bounds a command that stops answering.
until_ok() {
  local end=$((SECONDS + ${observation_wait:-20}))
  until "$@"; do
    ((SECONDS < end)) || {
      echo "FAIL waiting for $*" >&2
      return 1
    }
    sleep 0.1
  done
}
until_setup() { until_ok "$@"; }
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
    until_setup test -f "$repo/out/dev/flatpak/$LINUXAPK_INSTANCE/ended"
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
      # Position the window for absolute input without resizing Android's
      # display: resize snapshots temporarily block otherwise valid touches.
      if [[ ${LINUXAPK_TEST_COMPOSITOR:-kwin} == mutter ]]; then
        # HWC's first frame can precede Mutter creating its window actor.
        # Retry the compositor operation until it actually found the window.
        # shellcheck disable=SC2329 # invoked through until_ok
        positioned() {
          gdbus call --session --dest dev.linuxapk.TestDesktop --object-path /dev/linuxapk/TestDesktop \
            --method dev.linuxapk.TestDesktop.Position "$1" | grep -q true
        }
        until_ok positioned false
      else
        cat >"$T/input-window.js" <<'JS'
for (const w of workspace.windowList()) {
  if (String(w.resourceClass).includes("dev.linuxapk.probes")) {
    const geometry = w.frameGeometry;
    geometry.x = 300;
    geometry.y = 100;
    w.frameGeometry = geometry;
    workspace.activeWindow = w;
  }
}
JS
        script_id=$(gdbus call --session --dest org.kde.KWin --object-path /Scripting \
          --method org.kde.kwin.Scripting.loadScript "$T/input-window.js" linuxapk-input-window | grep -oE '[0-9]+')
        gdbus call --session --dest org.kde.KWin --object-path "/Scripting/Script$script_id" \
          --method org.kde.kwin.Script.run >/dev/null
        gdbus call --session --dest org.kde.KWin --object-path /Scripting \
          --method org.kde.kwin.Scripting.unloadScript linuxapk-input-window >/dev/null
      fi
      # Activity resume precedes InputDispatcher's focused-window publication.
      # Wait for the actual input target before submitting the burst.
      # shellcheck disable=SC2329 # invoked through until_ok
      input_ready() {
        s dumpsys input >"$T/devices.txt"
        sed -n '/^  FocusedWindows:/,/^  FocusRequests:/p' "$T/devices.txt" |
          grep -E "displayId=$1, name=.*${2:-InputProbeActivity}" >/dev/null &&
          grep -E "name=.*${2:-InputProbeActivity}, id=.*displayId=$1, .*alpha=1," "$T/devices.txt" |
          grep -Ev 'NOT_VISIBLE|NOT_FOCUSABLE' >/dev/null
      }
      until_ok input_ready "$display"
      file_has "$T/devices.txt" 'Linux.apk Keyboard'
      file_has "$T/devices.txt" 'Linux.apk Mouse'
      /usr/bin/python3 "$here/gamepads.py" check "$T/devices.txt"
      # Keep one virtual seat device across enter/leave and drag. Submit the
      # ordered stream without observation barriers between evdev devices.
      mkfifo "$T/seat"
      /usr/bin/python3 "$here/seat.py" --stdin <"$T/seat" &
      seat_pid=$!
      exec {seat_fd}>"$T/seat"
      # A submitted first frame precedes the compositor mapping/focusing the surface.
      # Establish real pointer delivery before the uninterrupted test stream.
      # shellcheck disable=SC2329 # invoked through until_ok
      pointer_ready() {
        printf 'abs:800,300\nabs:900,300\n' >&"$seat_fd"
        log_has LinuxApkProbe "generic .*ACTION_HOVER_(ENTER|MOVE).*deviceName=Linux.apk Mouse $display windowDisplay=$display"
      }
      until_ok pointer_ready
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
      until_ok input_ready "$second_display"
      second_x=1200
      if [[ ${LINUXAPK_TEST_COMPOSITOR:-kwin} == mutter ]]; then
        second_x=1800
        s logcat -b main -c
        until_ok positioned true
        # Place both at their original size: resize snapshots can consume valid
        # touches after hover is already delivered on a software renderer.
        # shellcheck disable=SC2329 # invoked through until_ok
        window_pointer_ready() {
          printf 'abs:%s,300\n' "$1" >&"$seat_fd"
          log_has LinuxApkProbe "generic .*ACTION_HOVER_(ENTER|MOVE).*deviceName=Linux.apk Mouse $2 windowDisplay=$2"
        }
        until_ok window_pointer_ready 200 "$display"
        until_ok window_pointer_ready "$second_x" "$second_display"
      else
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
      fi
      # Rapid alternating enter/move/click/leave reports must retain their own
      # display's device even with both endpoints readable.
      for _ in {1..2}; do
        printf 'abs:200,300\nbutton:272,1\nbutton:272,0\nabs:%s,300\nbutton:272,1\nbutton:272,0\n' "$second_x" >&"$seat_fd"
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
      echo 'ok   native mouse hover exit and modifier changes during drag; no duplicate modifier keys'
      # The two-window sequence changed size and focus; inject into a fresh mapped probe.
      launch InputProbeActivity
      until_ok mapped
      # The first frame is visible before the Android opening animation ends.
      # InputDispatcher drops otherwise valid injected touches at low opacity.
      until_ok input_ready "$display"
      harness=$(s pm path dev.linuxapk.probes | sed -n 's/^package://p' | tr -d '\r')
      sed "s/DISPLAY/$display/" "$here/system-app/probe-stream.txt" |
        s sh -c "CLASSPATH=$harness app_process / dev.linuxapk.probes.InputStdinHarness" >"$T/input.txt"
      until_ok log_has LinuxApkProbe 'key .*KEYCODE_A.*META_SHIFT_ON'
      until_ok log_has LinuxApkProbe 'touch .*ACTION_DOWN.*source=0x2002'
      until_ok log_has LinuxApkProbe 'generic .*ACTION_SCROLL'
      wm CLOSE --ei display "$display" | grep -q "closed display=$display"
      launch CaptureProbeActivity
      until_ok mapped
      until_ok input_ready "$display" CaptureProbeActivity
      printf '%s\n' abs:900,400 key:46,1 key:46,0 >&"$seat_fd"
      # One capture assertion: native relative motion reaches the requesting view.
      # shellcheck disable=SC2329 # invoked through until_ok
      captured_motion() {
        printf '%s\n' rel:20,-10 >&"$seat_fd"
        log_has LinuxApkCaptureProbe 'relative source=131076 x=20.0 y=-10.0 device=Linux.apk Mouse'
      }
      until_ok captured_motion
      printf '%s\n' key:19,1 key:19,0 >&"$seat_fd"
      exec {seat_fd}>&-
      wait "$seat_pid"
      rm "$T/seat"
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
      s am broadcast -n "dev.linuxapk.probes/.HandoffProbe\$HandlerControl" --ez enabled true >/dev/null
      # shellcheck disable=SC2329 # invoked by the EXIT trap
      reset_handler() { s am broadcast -n "dev.linuxapk.probes/.HandoffProbe\$HandlerControl" --ez enabled false >/dev/null; }
      trap reset_handler EXIT
      s am start -a android.intent.action.VIEW -d mailto:android-handler@example.com >/dev/null
      until_ok log_has LinuxApkHandoffProbe 'Android handler: mailto:android-handler@example.com'
      # NEW_TASK | CLEAR_TASK re-runs onCreate without killing the probe process.
      s am start -f 0x10008000 -n dev.linuxapk.probes/.HandoffProbe --es action view >/dev/null
      until_ok log_has LinuxApkHandoffProbe 'Android handler: content://dev.linuxapk.probes.handoff/shared.odt'
      reset_handler
      trap - EXIT
      s am broadcast -n "dev.linuxapk.probes/.NotificationProbe\$Receiver" -a dev.linuxapk.probes.NOTIFY >/dev/null
      until_ok file_has "$bridge_log" 'notify: .*not shown \(test session\)'
      for mode in progress style indeterminate style-indeterminate ongoing no-badge; do
        expected=42 badge=true
        case $mode in
          *indeterminate) expected=-2 ;;
          ongoing | no-badge) expected=-1 badge=false ;;
        esac
        s am broadcast -n "dev.linuxapk.probes/.NotificationProbe\$Receiver" -a dev.linuxapk.probes.NOTIFY --es mode "$mode" >/dev/null
        until_ok file_has "$bridge_log" "not shown .*Ada Lovelace $mode.*progress=$expected badge=$badge"
      done
      s am broadcast -n "dev.linuxapk.probes/.NotificationProbe\$Receiver" -a dev.linuxapk.probes.NOTIFY --es mode cancel >/dev/null
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
      # The cold fallback can finish its task before WM observes it. Explicitly
      # release this fixture's display before another same-size window opens.
      wm CLOSE --es package com.android.documentsui >/dev/null
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
  echo "e2e: deadline=${deadline}s (plus at most 4s cleanup)"
  timeout --kill-after=4 "$deadline" "$repo/tests/nested.sh" "$0" "${groups[@]}" &
  runner=$!
  trap 'kill -TERM "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true' INT TERM
  wait "$runner" || result=$?
  trap - INT TERM
  end=$(date +%s%N)
  /usr/bin/python3 -c 'import sys; print(f"e2e: TOTAL wall={(int(sys.argv[2])-int(sys.argv[1]))/1e9:.2f}s")' "$start" "$end"
  exit "$result"
fi
mkdir -p "$T"
: >"$T/timings.log"
export LINUXAPK_IDLE_GRACE=1 LINUXAPK_TEST_TIMERS=1 LINUXAPK_AUDIO_SINK=linuxapk_$LINUXAPK_INSTANCE
keeper=""
events=""
setup_lock=""
gamepads=""
mod=""
cleanup() {
  local result=$?
  if [[ -n $setup_lock ]]; then exec {setup_lock}>&-; fi
  if ((result != 0)); then
    for log in "$desktop_log" "$repo/out/dev/flatpak/$LINUXAPK_INSTANCE/runner.log" \
      "$repo/out/dev/flatpak/$LINUXAPK_INSTANCE/supervisor.log"; do
      if [[ -f $log ]]; then
        echo "e2e: last 60 lines of $log:" >&2
        tail -n 60 "$log" >&2
      fi
    done
    echo "e2e: Android failure diagnostics: $T/{crash,system}.txt" >&2
    timeout 1 "$S" exec logcat -d -b crash >"$T/crash.txt" 2>&1 || true
    if file_has "$T/crash.txt" 'FATAL EXCEPTION IN SYSTEM PROCESS'; then
      echo 'e2e: system_server crashed (not a boot readiness timeout):' >&2
      cat "$T/crash.txt" >&2
    fi
    # Keep boot's original error as well as a later recovery/watchdog failure.
    timeout 1 "$S" exec logcat -d -b main,system -t 10000 >"$T/system.txt" 2>&1 || true
  fi
  "$S" stop >/dev/null 2>&1 || true
  "$S" reset >/dev/null 2>&1 || true
  if [[ -n $events ]]; then wait "$events" 2>/dev/null || true; fi
  if [[ -n $keeper ]]; then wait "$keeper" 2>/dev/null || true; fi
  if [[ -n $gamepads ]]; then
    kill "$gamepads" 2>/dev/null || true
    wait "$gamepads" 2>/dev/null || true
  fi
  if [[ -n $mod ]]; then pactl unload-module "$mod"; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM
export LINUXAPK_GAMEPAD_TEST_DIR=$repo/out/dev/flatpak/$LINUXAPK_INSTANCE/gp
/usr/bin/python3 "$here/gamepads.py" serve "$LINUXAPK_GAMEPAD_TEST_DIR" &
gamepads=$!
until_ok test -f "$LINUXAPK_GAMEPAD_TEST_DIR/ready"
mod=$(pactl load-module module-null-sink sink_name="$LINUXAPK_AUDIO_SINK" rate=48000 channels=2)
timed setup-stop stop_session
timed setup-reset "$S" reset
mkdir -p "$(dirname "$bridge_log")"
: >"$bridge_log"
timed setup-desktop until_setup file_has "$desktop_log" ready
timed setup-start "$S" start
# The exec service is not ready immediately after start. Hold its existing
# busy lock from the host until a booted Android can run the keeper instead.
exec {setup_lock}>"$XDG_RUNTIME_DIR/app/dev.linuxapk.Manager/linuxapk-$LINUXAPK_INSTANCE/exec.lock"
flock --shared "$setup_lock"
timed setup-boot until_setup booted
: >"$T/keeper.log"
# Do not let the keeper inherit the host lock: lifecycle must stop the session
# after it releases its own exec lock by observing e2e-idle.
s sh -c 'echo ready; while ! test -f /data/local/tmp/e2e-idle; do sleep 0.1; done' \
  {setup_lock}>&- >"$T/keeper.log" 2>&1 &
keeper=$!
timed setup-keeper until_setup file_has "$T/keeper.log" '^ready$'
exec {setup_lock}>&-
setup_lock=""
size=$(stat -c %s "$apk")
timed setup-probes s cmd package install -r -g -S "$size" <"$apk"
# Keep the original process-death events if Android restarts and its noisy boot
# replaces the log ring before a failed group's snapshot. Tags, time and bytes
# are bounded; session stop closes this exec before cleanup waits for it.
(
  # shellcheck disable=SC2016 # expanded by the Android shell
  timeout "$deadline" "$S" exec sh -c '
    logcat -b main,system,crash -T 1 -v threadtime \
      -s Zygote:I AndroidRuntime:E ActivityManager:I ActivityTaskManager:I vold:I Process:I &
    logger=$!
    while ! test -f /data/local/tmp/e2e-events-stop; do sleep 0.1; done
    kill "$logger"
    wait "$logger" 2>/dev/null || true
  ' |
    head -c 2097152 >"$T/session-events.txt"
) &
events=$!
failed=()
run_group() {
  local result=0
  timed "$1" timeout --kill-after=2 "$(((50 * deadline + 54) / 55))" "$0" --group "$1" || result=$?
  if ((result != 0)); then
    # Preserve the failed activity sequence before lifecycle restarts logd.
    tail -n 200 "$bridge_log" >"$T/$1-bridge.log" 2>&1 || true
    timeout 1 "$S" exec logcat -d -t 1000 -s ActivityTaskManager:I ActivityManager:I LinuxApkHandoff:V >"$T/$1-activities.txt" 2>&1 || true
    timeout 1 "$S" exec logcat -d -b main,system,crash -t 10000 >"$T/$1-system.txt" 2>&1 || true
    if [[ -f $repo/out/dev/flatpak/$LINUXAPK_INSTANCE/cgroup ]]; then
      local cg stat
      cg=$(cat "$repo/out/dev/flatpak/$LINUXAPK_INSTANCE/cgroup")
      for stat in memory.events memory.peak memory.max; do
        if [[ -r $cg/$stat ]]; then
          printf '%s\n' "$stat"
          cat "$cg/$stat"
        fi
      done >"$T/$1-memory.txt"
    fi
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
  if [[ $group == lifecycle ]]; then
    # Logging is an exec too: release its busy lock before testing idle stop.
    s touch /data/local/tmp/e2e-events-stop
    wait "$events" || true
    events=""
    run_group lifecycle || failed+=(lifecycle)
  fi
done
((${#failed[@]} == 0)) || {
  echo "e2e: FAIL (${failed[*]})"
  exit 1
}
echo 'e2e: PASS'
