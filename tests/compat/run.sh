#!/usr/bin/env bash
# A windowed compat run always gets a disposable desktop.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
export LINUXAPK_PROFILE=${LINUXAPK_PROFILE:-compat}
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then
  for arg in "$@"; do
    if [[ $arg == --window ]]; then exec "$repo/tests/nested.sh" "$0" "$@"; fi
  done
fi
cd "$repo"
for arg in "$@"; do
  case $arg in fetch | report) exec cargo run -q --release -p linuxapk-compat -- "$@" ;; esac
done
# Keep compat sound off the desktop and give audio assertions a session-only
# monitor. An externally started session must use this same sink.
export LINUXAPK_AUDIO_SINK=${LINUXAPK_AUDIO_SINK:-linuxapk_compat_$LINUXAPK_PROFILE}
if ! pactl get-sink-volume "$LINUXAPK_AUDIO_SINK" >/dev/null 2>&1; then
  mod=$(pactl load-module module-null-sink sink_name="$LINUXAPK_AUDIO_SINK" rate=48000 channels=2)
  trap 'pactl unload-module "$mod"' EXIT
fi
cargo run -q --release -p linuxapk-compat -- "$@"
