#!/usr/bin/env bash
# Same-stage runtime comparison, with real app frame stats on isolated KWin.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-benchmark}
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then exec "$repo/tests/nested.sh" "$0" "$@"; fi
S=$repo/tools/dev/session.sh
T=$repo/out/test/benchmark/$LINUXAPK_INSTANCE
mkdir -p "$T"
cp "$repo/out/stage/share/linuxapk/manifest.tsv" "$T/manifest.tsv"
# shellcheck disable=SC2329 # invoked by EXIT
cleanup() {
  "$S" stop >/dev/null 2>&1 || true
  "$S" reset >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
boot() {
  "$S" wait
  local t=${EPOCHREALTIME/./}
  "$S" start
  until [[ $("$S" exec getprop sys.boot_completed 2>/dev/null) == 1 ]]; do
    ((${EPOCHREALTIME/./} - t < 300000000)) || {
      echo 'boot timed out'
      exit 1
    }
    sleep 0.1
  done
  echo "boot_ms=$(((${EPOCHREALTIME/./} - t) / 1000))"
}
boot | tee "$T/fresh.txt"
"$S" stop
boot | tee "$T/cold.txt"
sleep 90
# Without page cache, as compat measures it.
cg=$("$S" cgroup)
awk -v cur="$(cat "$cg/memory.current")" '$1 == "file" { f = $2 } $1 == "shmem" { s = $2 }
  END { print "idle_memory_mib=" int((cur - f + s) / 1048576) }' "$cg/memory.stat" | tee "$T/memory.txt"
for pkg in com.darkempire78.opencalculator org.wikipedia org.nsh07.pomodoro; do
  apk=$(find "$repo/out/test/compat/corpus/" -name "${pkg}_*.apk" -print -quit)
  size=$(stat -c %s "$apk")
  "$S" exec cmd package install -r -g -S "$size" <"$apk"
  act=$("$S" exec cmd package resolve-activity --brief "$pkg" | tail -1 | tr -d '\r')
  launch=$("$S" exec am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a dev.linuxapk.LAUNCH --es package "$pkg" --es component "$act")
  display=$(sed -n 's/.*launched display=\([0-9]*\).*/\1/p' <<<"$launch")
  [[ -n $display ]] || {
    echo "FAIL no window: $launch"
    exit 1
  }
  sleep 3
  # A fresh install: startup frames, then the same taps and scrolls each run.
  "$S" exec sh -c "for i in 1 2 3 4 5 6 7 8 9 10; do input -d $display tap 400 300; input -d $display swipe 600 650 600 150 500; input -d $display swipe 600 150 600 650 500; done"
  "$S" exec dumpsys gfxinfo "$pkg" framestats >"$T/gfxinfo-$pkg.txt"
  frames=$(sed -n '/^Total frames rendered:/ {s/^Total frames rendered: //; p; q;}' "$T/gfxinfo-$pkg.txt")
  [[ -n $frames && $frames -gt 0 ]] || {
    echo "FAIL no frame timing for $pkg"
    exit 1
  }
  echo "$pkg"
  sed -n '/^Total frames rendered:/,+9p' "$T/gfxinfo-$pkg.txt"
  "$S" exec am force-stop "$pkg"
done
