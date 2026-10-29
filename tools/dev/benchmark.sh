#!/usr/bin/env bash
# Same-stage runtime comparison, with real app frame stats on isolated KWin.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
export LINUXAPK_BINDER_TESTS=${LINUXAPK_BINDER_TESTS:-${XDG_DATA_HOME:-$HOME/.local/share}/linuxapk/test-bin/binder}
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-benchmark-${LINUXAPK_MODE:-flatpak}}
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then exec "$repo/tests/nested.sh" "$0" "$@"; fi
S=$repo/tools/dev/session.sh
T=$repo/out/test/benchmark/$LINUXAPK_INSTANCE
mkdir -p "$T"
# shellcheck disable=SC2329 # invoked by EXIT
cleanup() {
  "$S" stop >/dev/null 2>&1 || true
  "$S" reset >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
boot() {
  "$S" wait
  /usr/bin/python3 - "$S" <<'PY'
import subprocess, sys, time
start = time.monotonic()
subprocess.run([sys.argv[1], 'start'], check=True)
while subprocess.run([sys.argv[1], 'exec', 'getprop', 'sys.boot_completed'],
                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout.strip() != b'1':
    if time.monotonic() - start > 300:
        raise SystemExit('boot timed out')
    time.sleep(.1)
print(f'boot_ms={(time.monotonic() - start) * 1000:.0f}')
PY
}
boot | tee "$T/fresh.txt"
"$S" stop
boot | tee "$T/cold.txt"
sleep 90
cg=$("$S" cgroup)
/usr/bin/python3 - "$cg" <<'PY' | tee "$T/memory.txt"
from pathlib import Path
import sys
cg = Path(sys.argv[1])
stat = dict(line.split() for line in (cg / 'memory.stat').read_text().splitlines())
print('idle_memory_mib=' + str((int((cg / 'memory.current').read_text()) - int(stat['file']) + int(stat['shmem'])) // 1048576))
PY
binary=$LINUXAPK_BINDER_TESTS/binderThroughputTest
# Explicitly use the pinned corpus of platform test binaries.
(cd "$(dirname "$binary")" && sha256sum --quiet -c "$repo/supervisor/tests/binder/test-bin.sha256")
"$S" exec sh -c 'cat > /data/local/tmp/binderThroughputTest; chmod 755 /data/local/tmp/binderThroughputTest' <"$binary"
"$S" exec /data/local/tmp/binderThroughputTest -w 2 -i 20000 -p | tee "$T/binder.txt"
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
  # Fresh install: include startup frames and the same tap/scroll sequence.
  "$S" exec sh -c "for i in 1 2 3 4 5 6 7 8 9 10; do input -d $display tap 400 300; input -d $display swipe 600 650 600 150 500; input -d $display swipe 600 150 600 650 500; done"
  "$S" exec dumpsys gfxinfo "$pkg" framestats >"$T/gfxinfo-$pkg.txt"
  frames=$(sed -n 's/^Total frames rendered: //p' "$T/gfxinfo-$pkg.txt" | head -1)
  [[ -n $frames && $frames -gt 0 ]] || {
    echo "FAIL no frame timing for $pkg"
    exit 1
  }
  echo "$pkg"
  sed -n '/Total frames rendered:/,/HISTOGRAM:/p' "$T/gfxinfo-$pkg.txt" | head -n 10
  "$S" exec am force-stop "$pkg"
done
