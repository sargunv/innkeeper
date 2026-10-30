#!/usr/bin/env bash
# Manual 30 s capture proof on a private virtual source; not part of the e2e gate.
set -Eeuo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-capture-proof-$(date +%s%N)}
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then
  exec systemd-run --user --scope --quiet -p MemoryMax=1G -p TasksMax=1024 \
    "$repo/tests/nested.sh" "$0" "$@"
fi
cd "$repo"
S=$repo/tools/dev/session.sh
O=$repo/out/test/audio/$LINUXAPK_INSTANCE
[[ ! -e $O ]] || {
  echo "capture proof instance already exists: $LINUXAPK_INSTANCE" >&2
  exit 1
}
mkdir -p "$O"
export LINUXAPK_AUDIO_SINK=linuxapk_capture_$LINUXAPK_INSTANCE
export LINUXAPK_AUDIO_SOURCE=$LINUXAPK_AUDIO_SINK.monitor
module=$(pactl load-module module-null-sink sink_name="$LINUXAPK_AUDIO_SINK" rate=48000 channels=2)
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
apk=$repo/out/test/probes/LinuxApkProbes.apk
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
  --device="$LINUXAPK_AUDIO_SOURCE" >"$O/monitor.raw" &
monitor=$!
paplay --raw --format=s16le --rate=48000 --channels=1 \
  --device="$LINUXAPK_AUDIO_SINK" "$O/source.raw" >"$O/player.log" 2>&1 &
player=$!
"$S" exec am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LAUNCH \
  --es package dev.linuxapk.probes --es component dev.linuxapk.probes/.AudioCaptureProbe --ez wait true
end=$((SECONDS + 40))
until "$S" exec logcat -d -s CaptureProof:I | grep -q 'complete'; do
  ((SECONDS < end)) || {
    echo 'capture probe did not complete' >&2
    exit 1
  }
  sleep 0.1
done
"$S" exec logcat -d -t 200 -s CaptureProof:I >"$O/probe.log"
"$S" exec cat /data/user/0/dev.linuxapk.probes/files/capture.raw >"$O/capture.raw"
kill "$monitor"
wait "$monitor" || true
monitor=''
/usr/bin/python3 "$repo/tests/e2e/audio/continuity.py" "$O/monitor.raw" | tee "$O/monitor-continuity.json"
/usr/bin/python3 "$repo/tests/e2e/audio/continuity.py" "$O/capture.raw" | tee "$O/continuity.json"
