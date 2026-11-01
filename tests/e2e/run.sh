#!/usr/bin/env bash
# Run the e2e groups in one private desktop and session. Each group can be
# selected on its own and has a deadline.
set -Eeuo pipefail
# ERR also fires inside command substitutions, so report only top-level
# failures.
# shellcheck disable=SC2329 # invoked by the ERR trap
report_error() {
  if ((BASH_SUBSHELL == 0)); then
    echo "e2e: command failed (status=$1) at line $2: $3" >&2
  fi
}
trap 'report_error "$?" "$LINENO" "$BASH_COMMAND"' ERR
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
export INNKEEPER_PROFILE=${INNKEEPER_PROFILE:-e2e}
# OpenHarmony runs in its own profile. The Android groups reset their profile
# but keep its settings, and the settings choose the system.
if [[ $* == ohos && ${INNKEEPER_NESTED:-} != 1 ]]; then INNKEEPER_PROFILE+=-ohos; fi
export INNKEEPER_E2E=1
# The nested desktop provides the isolation, so the session runs in production
# mode. Its desktop requests reach desktop.py's fake portals and notifications.
unset INNKEEPER_TEST
S=$repo/tools/dev/session.sh
T=$repo/out/test/e2e/$INNKEEPER_PROFILE
desktop_log=$repo/out/test/nested/$INNKEEPER_PROFILE/desktop.log
bridge_log=$repo/out/test/nested/$INNKEEPER_PROFILE/state/innkeeper-$INNKEEPER_PROFILE/bridge.log
apk=$repo/out/test/probes/InnkeeperProbes.apk
# GitHub's software-rendered desktop is slower than local development.
deadline=${INNKEEPER_E2E_DEADLINE:-55}
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
wm() { /usr/bin/python3 "$here/wm.py" "$@"; }
# Poll for an observation and return as soon as it arrives. The group deadline
# also stops a command that hangs.
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
booted() { [[ $(s getprop sys.boot_completed 2>/dev/null) == 1 ]]; }
user_unlocked() { [[ $(s am get-started-user-state 0 2>/dev/null) == RUNNING_UNLOCKED ]]; }
log_has() { s logcat -d -s "$1:I" | grep -E "$2" >/dev/null; }
# mutter.js serves dev.sargunv.Innkeeper.TestDesktop on GNOME Shell, and kwin.py
# serves it on KWin.
desktop() {
  gdbus call --session --dest dev.sargunv.Innkeeper.TestDesktop --object-path /dev/sargunv/Innkeeper/TestDesktop \
    --method "dev.sargunv.Innkeeper.TestDesktop.$1" "${@:2}"
}
file_has() { grep -E "$2" "$1" >/dev/null; }
window_ready() { s dumpsys activity activities | grep "topResumedActivity=.*$1" >/dev/null; }
launch() {
  local out
  wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
  out=$(wm LAUNCH --es package dev.sargunv.innkeeper.probes --es component "dev.sargunv.innkeeper.probes/.$1")
  display=$(sed -n 's/.*launched display=\([0-9]*\).*/\1/p' <<<"$out")
  [[ -n $display ]] || {
    echo "FAIL launch: $out"
    return 1
  }
  if [[ $1 != DesktopProbe ]]; then until_ok window_ready "$1"; fi
}
stopped() { [[ $("$S" status) == stopped* ]]; }
running() { "$S" status >/dev/null; }
stop_session() { "$S" stop >/dev/null; }

if [[ ${1:-} == --group ]]; then
  case $2 in
    boot)
      # Check the real process tree, which matters even on hosts with Yama
      # disabled. Ubuntu's default ptrace_scope=1 allows memory access only
      # from an ancestor, so the broker must be one.
      # shellcheck disable=SC2016 # expanded inside Android
      s sh -c '
        for init in $(pidof init); do
          parent=$(sed -n "s/^PPid:[[:space:]]*//p" "/proc/$init/status")
          if test "$(cat /proc/$parent/comm)" = innkeeper-brk; then
            echo "init=$init parent=$parent (innkeeper-brk)"
            exit 0
          fi
        done
        exit 1
      ' >"$T/ancestry.txt"
      cat "$T/ancestry.txt"
      echo 'ok   Android init is a child of the broker'
      "$S" selftest
      # adb is off and requires authentication by default. Turning it on opens
      # a loopback port, announces it to a private adb server, and lets a
      # throwaway user key connect.
      [[ $(s getprop init.svc.adbd) != running && $(s getprop ro.adb.secure) == 1 ]]
      adb=$ANDROID_HOME/platform-tools/adb
      # The key pair stays in the test's directory. The public key is copied to
      # where the session looks for the user's key, in the dev sandbox's home.
      rm -rf "$T/adbhome" && mkdir -p "$T/adbhome/.android"
      "$adb" keygen "$T/adbhome/.android/adbkey" >/dev/null 2>&1
      keys=$repo/out/dev/flatpak/$INNKEEPER_PROFILE/home/.android
      mkdir -p "$keys"
      cp "$T/adbhome/.android/adbkey.pub" "$keys/"
      # Turning adb on announces the profile to this private adb server.
      A=(env HOME="$T/adbhome" "$adb" -P "$ANDROID_ADB_SERVER_PORT")
      "${A[@]}" start-server >/dev/null 2>&1
      "$repo/tools/dev/innkeeper.sh" profile set --adb on >"$T/adb-on.txt" 2>&1
      serial=$(grep -o 'emulator-[0-9]*' "$T/adb-on.txt")
      port=$((${serial#emulator-} + 1))
      # shellcheck disable=SC2329 # invoked through until_ok
      listening() { ss -ltnH "sport = :$port" >"$T/adb-listen.txt" && [[ -s $T/adb-listen.txt ]]; }
      until_ok listening
      if grep -v "127.0.0.1:$port" "$T/adb-listen.txt"; then
        echo "FAIL adb listens beyond loopback" >&2
        exit 1
      fi
      # shellcheck disable=SC2329 # invoked through until_ok
      adb_shell() { [[ $("${A[@]}" -s "$serial" shell echo ok 2>/dev/null) == ok ]]; }
      until_ok adb_shell
      "${A[@]}" kill-server
      "$repo/tools/dev/innkeeper.sh" profile set --adb off >/dev/null 2>&1
      echo "ok   adb is off by default; on, the adb server lists it and the user's key reaches it"
      driver=$(s getprop ro.hardware.vulkan)
      feature=android.hardware.vulkan.level=1
      # pm leaves out =0 when it prints a feature with version zero.
      if [[ $driver == lvp ]]; then feature=android.hardware.vulkan.level; fi
      s pm list features >"$T/features.txt"
      file_has "$T/features.txt" "^feature:$feature$"
      echo "ok   Vulkan feature matches $driver: $feature"
      # Session1 must report the same completed boot that the launcher's
      # property check sees.
      # shellcheck disable=SC2329 # invoked through until_ok
      session_booted() {
        gdbus call --session --dest "dev.sargunv.Innkeeper.Session.p_${INNKEEPER_PROFILE//-/_}" \
          --object-path /dev/sargunv/Innkeeper/Session --method dev.sargunv.Innkeeper.Session1.State >"$T/session-state.txt" &&
          file_has "$T/session-state.txt" '^\(true,'
      }
      until_ok session_booted
      booted
      echo 'ok   Session1 booted follows Android boot completion'
      s cmd package resolve-activity --brief -a android.intent.action.MAIN \
        -c android.intent.category.HOME >"$T/home.txt"
      file_has "$T/home.txt" '^dev.sargunv.innkeeper.system/.wm.HomeActivity$'
      s cmd appops set dev.sargunv.innkeeper.probes MANAGE_EXTERNAL_STORAGE allow
      s am broadcast -n dev.sargunv.innkeeper.probes/.StorageProbe >/dev/null
      until_ok log_has InnkeeperStorageProbe 'state=mounted app-dir=ok MediaStore=ok'
      # TeardownProbe finishes itself and handles no configuration changes. If
      # the window manager releases its display while task removal is in
      # flight, Android 17's display-compat recovery relaunches the activity
      # onto the removed display and crashes system_server (#50).
      syspid=$(s pidof system_server)
      am_start() { s am start -n "dev.sargunv.innkeeper.probes/.$1" >/dev/null; }
      am_start TeardownProbe
      until_ok log_has InnkeeperTeardownProbe 'created, display='
      until_ok log_has InnkeeperTeardownProbe 'self-finishing like the storage scan end'
      until_ok log_has InnkeeperTeardownProbe 'destroyed, display='
      # The display is released after close(), so wait until the probe's
      # windows and its "innkeeper:<package>" displays are gone before
      # comparing pids.
      # shellcheck disable=SC2329 # invoked through until_ok
      no_probe_windows() { ! wm LIST | grep -q 'package=dev.sargunv.innkeeper.probes .*window='; }
      # A failed or empty dumpsys must not count as the display being gone.
      # shellcheck disable=SC2329 # invoked through until_ok
      probe_display_gone() {
        local dump
        dump=$(s dumpsys display 2>/dev/null) || return 1
        [[ $dump == *'Display Devices:'* ]] || return 1
        ! grep -q 'innkeeper:' <<<"$dump"
      }
      until_ok no_probe_windows
      until_ok probe_display_gone
      [[ $(s pidof system_server) == "$syspid" ]] || {
        echo 'FAIL framework restarted while closing a probe window' >&2
        exit 1
      }
      echo 'ok   a self-finishing windowed probe closes without restarting the framework'
      ;;
    input)
      launch InputProbeActivity
      # shellcheck disable=SC2329 # invoked through until_ok
      mapped() { wm LIST | grep "display=$display package=dev.sargunv.innkeeper.probes .*window=yes" >/dev/null; }
      until_ok mapped
      # Position the window for absolute input without resizing Android's
      # display, because a resize snapshot briefly blocks valid touches. HWC's
      # first frame can arrive before the compositor has a window for it, so
      # retry until the compositor finds the window.
      # shellcheck disable=SC2329 # invoked through until_ok
      positioned() { desktop Position | grep -q true; }
      until_ok positioned
      # The activity resumes before InputDispatcher publishes it as the focused
      # window. Wait for that input target before sending the burst.
      # shellcheck disable=SC2329 # invoked through until_ok
      input_ready() {
        s dumpsys input >"$T/devices.txt"
        sed -n '/^  FocusedWindows:/,/^  FocusRequests:/p' "$T/devices.txt" |
          grep -E "displayId=$1, name=.*${2:-InputProbeActivity}" >/dev/null &&
          grep -E "name=.*${2:-InputProbeActivity}, id=.*displayId=$1, .*alpha=1," "$T/devices.txt" |
          grep -Ev 'NOT_VISIBLE|NOT_FOCUSABLE' >/dev/null
      }
      until_ok input_ready "$display"
      /usr/bin/python3 "$here/gamepads.py" check "$T/devices.txt"
      # The keyboard, click and capture scenarios share one native seat.
      mkfifo "$T/seat"
      /usr/bin/python3 "$here/seat.py" --stdin <"$T/seat" &
      seat_pid=$!
      exec {seat_fd}>"$T/seat"
      # The app submits its first frame before the compositor maps and focuses
      # the surface. Wait until pointer events reach the app before sending the
      # test events.
      # shellcheck disable=SC2329 # invoked through until_ok
      pointer_ready() {
        printf 'abs:800,300\nabs:900,300\n' >&"$seat_fd"
        log_has InnkeeperProbe "generic .*ACTION_HOVER_(ENTER|MOVE).*deviceName=Innkeeper Mouse $display windowDisplay=$display"
      }
      until_ok pointer_ready
      printf '%s\n' key:30,1 key:30,0 button:272,1 button:272,0 >&"$seat_fd"
      until_ok log_has InnkeeperProbe 'key .*KEYCODE_A.*deviceId=-1'
      until_ok log_has InnkeeperProbe "touch .*ACTION_UP.*deviceName=Innkeeper Mouse $display windowDisplay=$display"
      echo 'ok   native keyboard and mouse reach the app'
      launch CaptureProbeActivity
      until_ok mapped
      until_ok input_ready "$display" CaptureProbeActivity
      printf '%s\n' abs:900,400 key:46,1 key:46,0 >&"$seat_fd"
      # Native relative motion must reach the view that requested capture.
      # shellcheck disable=SC2329 # invoked through until_ok
      captured_motion() {
        printf '%s\n' rel:20,-10 >&"$seat_fd"
        log_has InnkeeperCaptureProbe 'relative source=131076 x=20.0 y=-10.0 device=Innkeeper Mouse'
      }
      until_ok captured_motion
      printf '%s\n' key:19,1 key:19,0 >&"$seat_fd"
      exec {seat_fd}>&-
      wait "$seat_pid"
      rm "$T/seat"
      launch WindowModeProbe
      # An orientation request through the public API must reshape the host
      # window.
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
      # Check the starting shape so that a window that never changes can't pass.
      until_ok shape portrait
      s am start --display "$display" -n dev.sargunv.innkeeper.probes/.WindowModeProbe --es action landscape >/dev/null
      until_ok shape landscape
      wm CLOSE --ei display "$display" >/dev/null
      echo 'ok   requested orientation reshapes the host window'
      /usr/bin/python3 "$here/app-settings.py"
      ;;
    audio)
      # The desktop mixer is the volume control, so a stream-volume request must
      # leave STREAM_MUSIC at the maximum index that dumpsys reports.
      s cmd media_session volume --stream 3 --set 1 >/dev/null || true
      audio_dump=$(s dumpsys audio)
      grep -q 'mUseFixedVolume=true' <<<"$audio_dump"
      /usr/bin/python3 -c '
import re, sys
dump = sys.argv[1]
block = re.search(r"- STREAM_MUSIC:\n(.*?)(?=\n- STREAM_|\Z)", dump, re.S)
body = block.group(1) if block else ""
max_index = re.search(r"Max: (\d+)", body)
current = re.search(r"Current: (.+)", body)
if (
    not max_index
    or not current
    or not re.search(rf": {max_index[1]}(,|$)", current[1])
):
    print("FAIL fixed volume: STREAM_MUSIC is not at its max index", file=sys.stderr)
    print(block.group(0) if block else "STREAM_MUSIC missing", file=sys.stderr)
    sys.exit(1)
' "$audio_dump"
      echo 'ok   stream volume stays at maximum; the desktop mixer is the control'
      parec -d "$INNKEEPER_AUDIO_SINK.monitor" --format=s16le --rate=48000 --channels=2 --raw >"$T/audio.raw" &
      recorder=$!
      trap 'kill "$recorder" 2>/dev/null || true; wait "$recorder" 2>/dev/null || true' EXIT
      s am start-foreground-service -n "dev.sargunv.innkeeper.probes/.LifecycleProbe\$Media" >/dev/null
      until_ok log_has InnkeeperLifecycleProbe 'foreground audio started'
      # The FFT succeeds once the capture has enough non-silent tone samples.
      until_ok /usr/bin/python3 "$here/audio/freq.py" "$T/audio.raw" --expect 440
      # am returns 1 when it successfully stops a service.
      s am stopservice -n "dev.sargunv.innkeeper.probes/.LifecycleProbe\$Media" >/dev/null || true
      launch BenchProbe
      s am start --display "$display" -n dev.sargunv.innkeeper.probes/.BenchProbe --es mode audio >/dev/null
      s am broadcast -a dev.sargunv.innkeeper.BENCH_BEGIN -p dev.sargunv.innkeeper.probes >/dev/null
      until_ok log_has InnkeeperBench 'BENCH audio .*roundtrip_ns=[1-9][0-9]*'
      echo 'ok   AudioRecord detected the known marker from the private sink monitor'
      wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
      ;;
    desktop)
      launch WideColorProbe
      # A host screenshot checks the pixels that the desktop displays, which can
      # differ from Android's buffer.
      # shellcheck disable=SC2329 # invoked through until_ok
      wide_colour_visible() {
        desktop Screenshot "$T/wide-colour.png" >/dev/null || return 1
        /usr/bin/python3 "$repo/tests/e2e/wide_colour.py" "$T/wide-colour.png"
      }
      until_ok wide_colour_visible
      echo 'ok   wide-colour window displays its SDR pixels on the host'
      # Change one setting live, saving and applying it as the manager does.
      "$repo/tools/dev/innkeeper.sh" profile set --follow-accent off >/dev/null
      until_ok log_has InnkeeperSettings 'desktop settings: .*followAccent=false'
      echo 'ok   profile set applies a desktop-following setting live'
      wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
      printf '# innkeeper-open-with\n' >"$T/file.md"
      "$repo/tools/dev/innkeeper.sh" open "$T/file.md"
      until_ok log_has InnkeeperOpenWith 'chooser type=text/markdown apps=dev.sargunv.innkeeper.probes'
      until_ok window_ready OpenWithActivity
      chooser=$(wm LIST | sed -n 's/.*display=\([0-9]*\) package=dev.sargunv.innkeeper.system .*/\1/p')
      s input -d "$chooser" keyevent KEYCODE_DPAD_DOWN KEYCODE_ENTER
      until_ok log_has InnkeeperOpenWith 'selected dev.sargunv.innkeeper.probes:'
      until_ok log_has InnkeeperFileProbe 'uri=content://dev.sargunv.innkeeper.system.documents/document/.*type=text/markdown read=true content=# innkeeper-open-with'
      until_ok window_ready FileProbe
      s logcat -d -s InnkeeperOpenWith:I InnkeeperFileProbe:I >"$T/open-with.txt"
      wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
      echo 'ok   Open With opens the selected app with a readable file grant'
      "$here/share.sh"
      s am start -n dev.sargunv.innkeeper.probes/.HandoffProbe --es action view >/dev/null
      until_ok file_has "$desktop_log" '^openfile: shared\.odt$'
      wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
      s am start -a android.intent.action.VIEW -d https://example.com/innkeeper-e2e >/dev/null
      until_ok file_has "$desktop_log" '^openuri: https://example.com/innkeeper-e2e$'
      s am start -n dev.sargunv.innkeeper.probes/.NotificationProbe >/dev/null
      until_ok file_has "$desktop_log" '^notification: Ada Lovelace *: Are we still on for the demo at 3\?'
      # The picker result test runs on Android's default display and needs no
      # window.
      s am start -n dev.sargunv.innkeeper.probes/.DesktopProbe >/dev/null
      until_ok file_has "$desktop_log" '^picker: Open File$'
      until_ok log_has InnkeeperDesktopProbe 'picked: innkeeper-e2e-file'
      wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
      echo 'ok   handoff, VIEW, notification and PICK reached the private desktop fakes'
      ;;
    lifecycle)
      launch BenchProbe
      until_ok log_has InnkeeperBench "BENCH resumed display=$display"
      # shellcheck disable=SC2329 # invoked through until_ok
      bench_mapped() { wm LIST | grep -q "display=$display package=dev.sargunv.innkeeper.probes .*window=yes"; }
      until_ok bench_mapped
      resumes() { s logcat -d -s InnkeeperBench:I | grep -c "BENCH resumed display=$display"; }
      before=$(resumes)
      until_ok /usr/bin/python3 "$here/windows.py" minimize
      until_ok log_has InnkeeperBench "BENCH stopped display=$display"
      # The activity must not resume while the window is minimized.
      [[ $(resumes) -eq $before ]]
      before=$(resumes)
      until_ok /usr/bin/python3 "$here/windows.py" restore
      # shellcheck disable=SC2329 # invoked through until_ok
      resumed_again() { [[ $(resumes) -gt $before ]]; }
      until_ok resumed_again
      echo 'ok   minimizing stops the activity and restoring resumes the same display'
      launch WindowProbe
      s am broadcast -a dev.sargunv.innkeeper.WINDOW_PROBE -p dev.sargunv.innkeeper.probes --es mode flag --ez enabled true >/dev/null
      until_ok file_has "$bridge_log" "window $display idle-inhibit on"
      s am broadcast -a dev.sargunv.innkeeper.WINDOW_PROBE -p dev.sargunv.innkeeper.probes --es mode flag --ez enabled false >/dev/null
      until_ok file_has "$bridge_log" "window $display idle-inhibit off"
      echo 'ok   a visible keep-screen-on request inhibits desktop idle until cleared'
      launch TaskWindowProbe
      first=$display
      # shellcheck disable=SC2329 # invoked through until_ok
      first_task_mapped() { wm LIST | grep -q "display=$first package=dev.sargunv.innkeeper.probes .*window=yes"; }
      until_ok first_task_mapped
      # An app's New Window starts another task on the caller's display. Both
      # tasks must get their own toplevel, and closing one must leave the other
      # task and its window alive.
      s am start --display "$first" --activity-single-top \
        -n dev.sargunv.innkeeper.probes/.TaskWindowProbe --ez new-window true >/dev/null
      # shellcheck disable=SC2329 # invoked through until_ok
      two_task_windows() {
        wm LIST >"$T/task-windows.txt"
        [[ $(grep -c 'package=dev.sargunv.innkeeper.probes .*window=yes' "$T/task-windows.txt") == 2 ]]
      }
      until_ok two_task_windows
      second=$(sed -n 's/.*display=\([0-9]*\) package=dev.sargunv.innkeeper.probes .*/\1/p' "$T/task-windows.txt" | grep -vx "$first")
      wm CLOSE --ei display "$first" >/dev/null
      # shellcheck disable=SC2329 # invoked through until_ok
      second_task_survives() {
        wm LIST >"$T/task-survivor.txt"
        ! grep -q "display=$first package=" "$T/task-survivor.txt" &&
          grep -Eq "display=$second package=dev.sargunv.innkeeper.probes .*tasks=[0-9]+ window=yes" "$T/task-survivor.txt"
      }
      until_ok second_task_survives
      wm CLOSE --ei display "$second" >/dev/null
      echo 'ok   a second task has its own toplevel and survives closing the first'
      s cmd appops set dev.sargunv.innkeeper.probes SCHEDULE_EXACT_ALARM allow
      # The ten-second alarm leaves four seconds to observe the idle stop before
      # the wakeup starts six seconds ahead of the alarm.
      s am start -n dev.sargunv.innkeeper.probes/.LifecycleProbe --es action alarm --ei seconds 10 >/dev/null
      until_ok log_has InnkeeperLifecycleProbe 'alarm scheduled at='
      wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
      s touch /data/local/tmp/e2e-idle
      until_ok stopped
      echo 'ok   session stopped idle with an alarm pending'
      until_ok running
      until_ok booted
      # The portal started this sandbox instead of the dev runner, and the
      # drop-in must still cap its scope.
      cg=$("$S" cgroup)
      [[ $(<"$cg/memory.max") == $((8 << 30)) && $(<"$cg/memory.high") == $((6 << 30)) ]]
      echo "ok   the alarm wakeup's sandbox scope is memory capped (${cg##*/})"
      ;;
  esac
  echo "ok   $2"
  exit
fi

groups=("$@")
((${#groups[@]})) || groups=(boot input audio desktop lifecycle)
if [[ ${groups[*]} != ohos ]]; then
  for group in "${groups[@]}"; do
    case $group in boot | input | audio | desktop | lifecycle) ;; *)
      echo "unknown e2e group: $group (ohos runs alone)" >&2
      exit 2
      ;;
    esac
  done
elif [[ ${INNKEEPER_NESTED:-} != 1 ]]; then
  # Fetching the image is setup and doesn't count toward the deadline.
  st=$repo/out/stage
  "$st/libexec/innkeeper/image/fetch.sh" ohos "$st/share/innkeeper/ohos/pin.toml" \
    "${INNKEEPER_IMAGES:-${XDG_DATA_HOME:-$HOME/.local/share}/innkeeper/images}" >/dev/null
else
  # OpenHarmony runs as a separate session in a fresh profile. The CLI creates
  # the profile, the launcher starts it as it starts Android, and its window
  # appears on this desktop.
  mkdir -p "$T"
  : >"$T/timings.log"
  cli=$repo/tools/dev/innkeeper.sh
  stop_session
  "$cli" profile delete "$INNKEEPER_PROFILE" >/dev/null 2>&1 || true
  "$cli" profile create --system ohos "$INNKEEPER_PROFILE" >/dev/null
  trap stop_session EXIT
  # shellcheck disable=SC2329 # invoked through timed
  ohos_boot() {
    "$S" start >/dev/null
    # The exec relay starts with OpenHarmony's post-fs-data.
    until s /system/bin/sh -c : 2>/dev/null; do sleep 0.1; done
    # The caller tests this function's status, which turns off errexit here.
    s /system/bin/sh -s <"$repo/ohos/tests/boot.sh" >"$T/ohos.txt" 2>&1 || return
    "$S" selftest >>"$T/ohos.txt" || return
    # Screen 0 must appear in its own window on this desktop.
    until_ok file_has "$bridge_log" 'window [0-9]+ "OpenHarmony" \(dev\.sargunv\.innkeeper\.ohos\)' || return
    until_ok file_has "$bridge_log" 'window [0-9]+ shows its first frame'
  }
  if ! timed ohos ohos_boot; then
    tail -n 60 "$T/ohos.txt" >&2
    echo "e2e: FAIL (ohos); init's log: $(dirname "$bridge_log")/kmsg.log" >&2
    exit 1
  fi
  echo 'ok   OpenHarmony registers its core system abilities, starts SceneBoard, shows screen 0 in a desktop window and passes selftest'
  # Each animation from here on draws in render_service, which must not
  # restart.
  ohos_rs=$(s /system/bin/pidof render_service)
  # shellcheck disable=SC2329 # invoked through timed
  ohos_input() {
    local x y w h panel pw ph cw ch left top
    locked() { s /system/bin/hidumper -s ScreenlockService -a -all | grep -Eq "screenLocked[[:space:]]+$1"; }
    settings() {
      s /system/bin/hidumper -s AbilityManagerService -a -a >"$T/abilities.txt"
      grep -A3 'main name \[com.ohos.settings.MainAbility\]' "$T/abilities.txt" | grep -Eq "^ *state #$1"
    }
    framed() {
      read -r x y w h < <(desktop Geometry dev.sargunv.innkeeper.ohos | tr -d '(),')
      ((w > 0))
    }
    until_ok framed || return
    # The window shows the panel at the window's scale, with the frame's
    # title bar above it. OpenHarmony's coordinates are panel pixels.
    panel=$(s /system/bin/param get const.innkeeper.display)
    pw=${panel%%x*} ph=${panel#*x} ph=${ph%%@*}
    read -r cw ch < <(grep -Eo '"OpenHarmony" \(dev\.sargunv\.innkeeper\.ohos\) [0-9]+x[0-9]+' "$bridge_log" |
      tail -n 1 | sed 's/.* //; s/x/ /')
    left=$((x + (w - cw) / 2)) top=$((y + h - ch - (w - cw) / 2))
    at() { echo "abs:$((left + $1 * cw / pw)),$((top + $2 * ch / ph))"; }
    until_ok locked true || return
    # A drag up the lock screen unlocks it.
    {
      at $((pw / 2)) $((ph * 9 / 10))
      echo button:272,1
      for i in 8 7 6 5 4 3 2; do at $((pw / 2)) $((ph * i / 10)); done
      echo button:272,0
    } | /usr/bin/python3 "$here/seat.py" --stdin
    until_ok locked false || return
    # A click on the home screen's Settings icon opens Settings. The home
    # screen ignores clicks while it animates in after the unlock.
    ! settings '[A-Z]+' || return
    click_settings() {
      { at 90 200 && echo button:272,1 && echo button:272,0; } | /usr/bin/python3 "$here/seat.py" --stdin
      settings '[A-Z]+'
    }
    until_ok click_settings || return
    until_ok settings FOREGROUND
  }
  if ! timed ohos-input ohos_input; then
    echo "e2e: FAIL (ohos-input); render_service: $(s /system/bin/pidof render_service)" >&2
    exit 1
  fi
  echo 'ok   desktop pointer input unlocks the lock screen and opens Settings'
  # shellcheck disable=SC2329 # invoked through timed
  ohos_render() {
    s /system/bin/sh -s "$ohos_rs" <"$repo/ohos/tests/render.sh" >>"$T/ohos.txt" 2>&1
  }
  if ! timed ohos-render ohos_render; then
    tail -n 5 "$T/ohos.txt" >&2
    exit 1
  fi
  echo 'ok   render_service draws the unlock, Settings and its scrolling without a restart'
  # shellcheck disable=SC2329 # invoked through timed
  ohos_keys() {
    # MMI logs each key with its device, OpenHarmony's key code and state.
    # KEY_A is OpenHarmony's KEYCODE_A, 2017.
    keyed() {
      s /system/bin/hidumper -s MultimodalInput -a -e |
        grep -q 'deviceId:0,sourceType:[a-z-]*,keyCode:2017,keyAction:key_action_up'
    }
    ! keyed || return
    printf '%s\n' key:30,1 key:30,0 | /usr/bin/python3 "$here/seat.py" --stdin
    until_ok keyed
  }
  if ! timed ohos-keys ohos_keys; then
    exit 1
  fi
  echo "ok   desktop keys reach OpenHarmony's input service"
  # shellcheck disable=SC2329 # invoked through timed
  ohos_hap() {
    # The probe HAP (ohos/probe) is signed with a debug profile for this
    # profile's UDID, which profile list shows. The CLI installs it, lists it
    # and starts its launcher ability, which comes to the foreground over the
    # home screen.
    probe() {
      s /system/bin/hidumper -s AbilityManagerService -a -a >"$T/abilities.txt"
      grep -A5 'app name \[dev.sargunv.innkeeper.probes\]' "$T/abilities.txt" | grep -Eq "^ *state #$1"
    }
    local udid probes=$repo/out/test/probes
    udid=$("$cli" profile list | awk -v p="$INNKEEPER_PROFILE" '$1 == p && $5 == "udid" { print $6 }')
    "$repo/ohos/probe/sign.sh" "$probes/ohos-sign" dev.sargunv.innkeeper.probes "$udid" \
      "$probes/InnkeeperProbes.hap" "$T/probes.hap" >>"$T/ohos.txt" 2>&1 || return
    "$cli" install "$T/probes.hap" >>"$T/ohos.txt" 2>&1 || return
    grep -q 'installed dev.sargunv.innkeeper.probes as "Innkeeper Probes"' "$T/ohos.txt" || return
    "$cli" list | grep '^dev\.sargunv\.innkeeper\.probes' >/dev/null || return
    "$cli" run dev.sargunv.innkeeper.probes >>"$T/ohos.txt" 2>&1 || return
    until_ok probe FOREGROUND
  }
  if ! timed ohos-hap ohos_hap; then
    tail -n 20 "$T/ohos.txt" >&2
    exit 1
  fi
  echo 'ok   the CLI installs, lists and starts the debug-signed probe HAP, which comes to the foreground'
  echo 'e2e: PASS'
  exit
fi
if [[ ${INNKEEPER_NESTED:-} != 1 ]]; then
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
export INNKEEPER_TEST_TIMERS=1 INNKEEPER_AUDIO_SINK=innkeeper_$INNKEEPER_PROFILE
# adb announcements go to a private server instead of the user's server on 5037.
export ANDROID_ADB_SERVER_PORT=15990
export INNKEEPER_AUDIO_SOURCE=$INNKEEPER_AUDIO_SINK.monitor
keeper=""
events=""
setup_lock=""
gamepads=""
mod=""
cleanup() {
  local result=$?
  if [[ -n $setup_lock ]]; then exec {setup_lock}>&-; fi
  if ((result != 0)); then
    for log in "$desktop_log" "$repo/out/dev/flatpak/$INNKEEPER_PROFILE/runner.log" \
      "$repo/out/dev/flatpak/$INNKEEPER_PROFILE/supervisor.log"; do
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
    # Keep the original boot error as well as any later recovery or watchdog
    # failure.
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
export INNKEEPER_GAMEPAD_TEST_DIR=$repo/out/dev/flatpak/$INNKEEPER_PROFILE/gp
/usr/bin/python3 "$here/gamepads.py" serve "$INNKEEPER_GAMEPAD_TEST_DIR" &
gamepads=$!
until_ok test -f "$INNKEEPER_GAMEPAD_TEST_DIR/ready"
mod=$(pactl load-module module-null-sink sink_name="$INNKEEPER_AUDIO_SINK" rate=48000 channels=2)
timed setup-stop stop_session
timed setup-reset "$S" reset
mkdir -p "$(dirname "$bridge_log")"
: >"$bridge_log"
timed setup-desktop until_ok file_has "$desktop_log" ready
timed setup-start "$S" start
# The exec service isn't ready right after start. Hold its busy lock from the
# host until Android has booted and can run the keeper, which then holds the
# lock.
exec {setup_lock}>"$XDG_RUNTIME_DIR/app/dev.sargunv.Innkeeper/innkeeper-$INNKEEPER_PROFILE/exec.lock"
flock --shared "$setup_lock"
timed setup-boot until_ok booted
# On the software-rendered runner, BOOT_COMPLETED can arrive before the CE app
# data is prepared. An install during unlock can succeed without creating the
# app's data directories.
timed setup-user-unlock until_ok user_unlocked
: >"$T/keeper.log"
# The keeper must not inherit the host lock. The lifecycle group stops the
# session after the keeper sees e2e-idle and releases its own exec lock.
s sh -c 'echo ready; while ! test -f /data/local/tmp/e2e-idle; do sleep 0.1; done' \
  {setup_lock}>&- >"$T/keeper.log" 2>&1 &
keeper=$!
timed setup-keeper until_ok file_has "$T/keeper.log" '^ready$'
exec {setup_lock}>&-
setup_lock=""
size=$(stat -c %s "$apk")
timed setup-probes s cmd package install -r -g -S "$size" <"$apk"
# Record process-death events as they happen, because if Android restarts, its
# noisy boot can overwrite the log ring before a failed group takes its
# snapshot. The tags, duration and size are bounded, and session stop closes
# this exec before cleanup waits for it.
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
    # Save the failed activity sequence before the lifecycle group restarts logd.
    tail -n 200 "$bridge_log" >"$T/$1-bridge.log" 2>&1 || true
    timeout 1 "$S" exec logcat -d -t 1000 -s ActivityTaskManager:I ActivityManager:I InnkeeperHandoff:V >"$T/$1-activities.txt" 2>&1 || true
    timeout 1 "$S" exec logcat -d -b main,system,crash -t 10000 >"$T/$1-system.txt" 2>&1 || true
    local cg stat
    if cg=$("$S" cgroup); then
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
# AudioRecord needs a visible activity. The audio group runs after the window
# and input checks, so it doesn't share a foreground window with them, and
# before the lifecycle group stops the session.
for group in "${groups[@]}"; do
  case $group in audio | lifecycle) continue ;; esac
  run_group "$group" || failed+=("$group")
done
for group in "${groups[@]}"; do
  if [[ $group == audio ]]; then run_group audio || failed+=(audio); fi
done
for group in "${groups[@]}"; do
  if [[ $group == lifecycle ]]; then
    # The logger is also an exec and holds a busy lock, so stop it before
    # testing the idle stop.
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
