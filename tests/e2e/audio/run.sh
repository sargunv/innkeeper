#!/usr/bin/env bash
# Audio end-to-end proof: Android -> audio HAL -> our libtinyalsav2 -> bridge
# -> PipeWire, captured from a private null sink's monitor (nothing plays on
# the desktop's speakers).
#
#   tests/e2e/audio/run.sh [--game PKG]   (default game: io.anuke.mindustry)
#
# 1. Loads a null sink "linuxapk_test" (unloaded at the end).
# 2. Starts a headless session instance (LINUXAPK_INSTANCE, default
#    "audio-e2e") routed to that sink, unless one is running.
# 3. Plays a 440 Hz tone with linuxapk-tone (AAudio, low-latency request)
#    and checks the capture's dominant frequency with freq.py; prints the
#    app-visible output latency.
# 4. Installs and launches a game from out/test/compat/corpus and checks that
#    the capture during its title screen is not silent.
# Leaves the session running (stop: LINUXAPK_INSTANCE=...
# mise run session -- stop).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
corpus=$repo/out/test/compat/corpus
tone=$repo/out/build/android/tests/e2e/audio/linuxapk-tone
game=io.anuke.mindustry
[[ ${1:-} == --game ]] && game=$2
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-audio-e2e}
export LINUXAPK_AUDIO_SINK=linuxapk_test
S=$repo/tools/dev/session.sh
T=$repo/out/test/audio
[[ -x $S && -f $tone ]] || {
  echo "no staged session or $tone (run \`mise run build\`)" >&2
  exit 1
}
mkdir -p "$T"
fail=0

mod=""
if ! pactl list short sinks | grep -q "\blinuxapk_test\b"; then
  mod=$(pactl load-module module-null-sink sink_name=linuxapk_test \
    sink_properties=device.description=Linux.apk-test rate=48000 channels=2)
fi
# shellcheck disable=SC2329 # invoked by the trap
cleanup() { if [[ -n $mod ]]; then pactl unload-module "$mod"; fi; }
trap cleanup EXIT

record() { # seconds file
  timeout "$1" parec -d linuxapk_test.monitor --format=s16le --rate=48000 --channels=2 \
    --raw --latency-msec=20 >"$2" || true
}

if ! "$S" status >/dev/null 2>&1; then
  if [[ ${LINUXAPK_NESTED:-} == 1 ]]; then "$S" start; else "$S" start --headless; fi
fi
for _ in $(seq 120); do
  [[ $(timeout 10 "$S" exec getprop sys.boot_completed 2>/dev/null) == 1 ]] && break
  sleep 2
done
timeout 20 "$S" exec cmd media_session volume --stream 3 --set 15 >/dev/null 2>&1 || true

echo "== 440 Hz tone (AAudio)"
"$S" exec sh -c 'cat > /data/local/tmp/linuxapk-tone && chmod 755 /data/local/tmp/linuxapk-tone' \
  <"$tone"
record 7 "$T/tone.raw" &
sleep 0.5
timeout 30 "$S" exec /data/local/tmp/linuxapk-tone 5 440 1 | tee "$T/tone.txt"
wait
python3 "$here/freq.py" "$T/tone.raw" --expect 440 | tee "$T/tone-freq.txt" || fail=1

echo "== game: $game"
apk=$(find "$corpus/" -maxdepth 1 -name "${game}_*.apk" | head -1)
if [[ -z $apk ]]; then
  echo "no $game APK in $corpus (mise run test:compat -- fetch)"
  fail=1
else
  "$S" exec sh -c 'cat > /data/local/tmp/e2e-game.apk' <"$apk"
  timeout 180 "$S" exec pm install -r -g /data/local/tmp/e2e-game.apk >/dev/null
  act=$(timeout 20 "$S" exec cmd package resolve-activity --brief "$game" | tail -1)
  timeout 60 "$S" exec am start -W -n "$act" >/dev/null
  sleep 15
  record 10 "$T/game.raw"
  python3 "$here/freq.py" "$T/game.raw" --min-dbfs -60 | tee "$T/game-freq.txt" || {
    echo "FAIL: silent capture"
    fail=1
  }
  timeout 20 "$S" exec am force-stop "$game" || true
fi
((fail == 0)) && echo "e2e: PASS" || echo "e2e: FAIL"
exit "$fail"
