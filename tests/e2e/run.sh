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
    boot) "$S" selftest ;;
    input)
      launch InputProbeActivity
      # shellcheck disable=SC2329 # invoked through until_ok
      mapped() { wm LIST | grep "display=$display package=dev.linuxapk.probes .*window=yes" >/dev/null; }
      until_ok mapped
      harness=$(s pm path dev.linuxapk.probes | sed -n 's/^package://p' | tr -d '\r')
      sed "s/DISPLAY/$display/" "$here/system-app/probe-stream.txt" |
        s sh -c "CLASSPATH=$harness app_process / dev.linuxapk.probes.InputStdinHarness" >"$T/input.txt"
      until_ok log_has LinuxApkProbe 'key .*KEYCODE_A.*META_SHIFT_ON'
      until_ok log_has LinuxApkProbe 'touch .*ACTION_DOWN.*source=0x2002'
      until_ok log_has LinuxApkProbe 'generic .*ACTION_SCROLL'
      wm CLOSE --ei display "$display" | grep -q "closed display=$display"
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
      ;;
    a11y)
      dbus-send --session --dest=org.a11y.Bus --type=method_call --print-reply /org/a11y/bus \
        org.freedesktop.DBus.Properties.Set string:org.a11y.Status string:IsEnabled variant:boolean:true >/dev/null
      launch A11yProbeActivity
      until_ok file_has "$bridge_log" 'a11y: test session: never active'
      wm CLOSE --es package dev.linuxapk.probes >/dev/null
      ;;
    lifecycle)
      s cmd appops set dev.linuxapk.probes SCHEDULE_EXACT_ALARM allow
      s logcat -c
      s am start -n dev.linuxapk.probes/.LifecycleProbe --es action alarm --ei seconds 18 >/dev/null
      until_ok log_has LinuxApkLifecycleProbe 'alarm scheduled at='
      wm CLOSE --es package dev.linuxapk.probes >/dev/null
      s touch /data/local/tmp/e2e-idle
      until_ok stopped
      echo 'ok   session stopped idle with an alarm pending'
      until_ok running
      until_ok booted
      until_ok log_has LinuxApkLifecycleProbe 'alarm fired at='
      ;;
  esac
  echo "ok   $2"
  exit
fi

groups=("$@")
((${#groups[@]})) || groups=(boot input audio desktop a11y lifecycle)
for group in "${groups[@]}"; do
  case $group in boot | input | audio | desktop | a11y | lifecycle) ;; *)
    echo "unknown e2e group: $group" >&2
    exit 2
    ;;
  esac
done
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then
  start=$(date +%s%N)
  result=0
  timeout --kill-after=5 52 "$repo/tests/nested.sh" "$0" "${groups[@]}" &
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
  return "$result"
}
# Audio uses a foreground service and a private sink; it can run alongside
# the sequential activity/window checks. Lifecycle changes session state last.
audio_pid=""
for group in "${groups[@]}"; do
  if [[ $group == audio ]]; then
    run_group audio &
    audio_pid=$!
  fi
done
for group in "${groups[@]}"; do
  case $group in audio | lifecycle) continue ;; esac
  run_group "$group" || failed+=("$group")
done
if [[ -n $audio_pid ]]; then wait "$audio_pid" || failed+=(audio); fi
for group in "${groups[@]}"; do
  if [[ $group == lifecycle ]]; then run_group lifecycle || failed+=(lifecycle); fi
done
((${#failed[@]} == 0)) || {
  echo "e2e: FAIL (${failed[*]})"
  exit 1
}
echo 'e2e: PASS'
