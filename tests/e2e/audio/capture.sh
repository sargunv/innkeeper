#!/usr/bin/env bash
# Manual 30 s capture proof on a private virtual source; not part of the e2e gate.
set -Eeuo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
export INNKEEPER_PROFILE=${INNKEEPER_PROFILE:-capture-proof-$(date +%s%N)}
if [[ ${INNKEEPER_NESTED:-} != 1 ]]; then
  exec systemd-run --user --scope --quiet -p MemoryMax=1G -p TasksMax=1024 \
    "$repo/tests/nested.sh" "$0" "$@"
fi
cd "$repo"
S=$repo/tools/dev/session.sh
O=$repo/out/test/audio/$INNKEEPER_PROFILE
[[ ! -e $O ]] || {
  echo "capture proof profile already exists: $INNKEEPER_PROFILE" >&2
  exit 1
}
mkdir -p "$O"
export INNKEEPER_AUDIO_SINK=innkeeper_capture_$INNKEEPER_PROFILE
export INNKEEPER_AUDIO_SOURCE=$INNKEEPER_AUDIO_SINK.monitor
module=$(pactl load-module module-null-sink sink_name="$INNKEEPER_AUDIO_SINK" rate=48000 channels=2)
keeper='' player='' monitor=''
cleanup() {
  [[ -z $monitor ]] || kill "$monitor" 2>/dev/null || true
  [[ -z $player ]] || kill "$player" 2>/dev/null || true
  [[ -z $keeper ]] || kill "$keeper" 2>/dev/null || true
  "$S" stop >/dev/null 2>&1 || true
  pactl unload-module "$module" || true
}
trap cleanup EXIT
"$S" start
end=$((SECONDS + 20))
until [[ $("$S" exec getprop sys.boot_completed 2>/dev/null) == 1 &&
$("$S" exec am get-started-user-state 0 2>/dev/null) == RUNNING_UNLOCKED ]]; do
  ((SECONDS < end)) || {
    echo 'capture session failed to boot' >&2
    exit 1
  }
  sleep 0.1
done
"$S" exec sh -c 'echo ready; sleep 60' >"$O/keeper.log" &
keeper=$!
testbin=$repo/out/build/android/android/vendor/audio/test-bin/audio_input_test
"$S" exec sh -c 'cat > /data/local/tmp/audio_input_test; chmod 700 /data/local/tmp/audio_input_test' <"$testbin"
"$S" exec /data/local/tmp/audio_input_test | tee "$O/input-test.log"
apk=$repo/out/test/probes/InnkeeperProbes.apk
apk_size=$(stat -c %s "$apk")
"$S" exec cmd package install -r -g -S "$apk_size" <"$apk"
/usr/bin/python3 - "$O/source.raw" <<'PY'
import array
import math
import sys
second = array.array("h", (round(10000 * math.sin(i * 2 * math.pi * 440 / 48000)) for i in range(48000)))
if sys.byteorder != "little":
    second.byteswap()
with open(sys.argv[1], "wb") as output:
    for _ in range(45):
        second.tofile(output)
PY
parec --raw --format=s16le --rate=48000 --channels=1 \
  --device="$INNKEEPER_AUDIO_SOURCE" >"$O/monitor.raw" &
monitor=$!
paplay --raw --format=s16le --rate=48000 --channels=1 \
  --device="$INNKEEPER_AUDIO_SINK" "$O/source.raw" >"$O/player.log" 2>&1 &
player=$!
"$S" exec am broadcast -n dev.sargunv.innkeeper.system/.wm.ControlReceiver -a dev.sargunv.innkeeper.LAUNCH \
  --es package dev.sargunv.innkeeper.probes --es component dev.sargunv.innkeeper.probes/.AudioCaptureProbe --ez wait true
end=$((SECONDS + 40))
until "$S" exec logcat -d -s CaptureProof:I | grep -q 'complete'; do
  ((SECONDS < end)) || {
    echo 'capture probe did not complete' >&2
    exit 1
  }
  sleep 0.1
done
"$S" exec logcat -d -t 200 -s CaptureProof:I >"$O/probe.log"
"$S" exec cat /data/user/0/dev.sargunv.innkeeper.probes/files/capture.raw >"$O/capture.raw"
"$S" exec dumpsys media.audio_flinger >"$O/audio-flinger.txt"
"$S" exec dumpsys media.audio_policy >"$O/audio-policy.txt"
kill "$monitor"
wait "$monitor" || true
monitor=''
/usr/bin/python3 "$repo/tests/e2e/audio/continuity.py" "$O/monitor.raw" | tee "$O/monitor-continuity.json"
/usr/bin/python3 "$repo/tests/e2e/audio/continuity.py" "$O/capture.raw" | tee "$O/continuity.json"
