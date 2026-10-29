#!/usr/bin/env bash
# Boot a fresh headless test instance (LINUXAPK_INSTANCE, default boot; its
# /data under out/test/boot), wait for Android to finish booting, run the
# sandbox selftest, and stop it again.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
session=$repo/tools/dev/session.sh
[[ -x $session ]] || {
  echo "no $session (run \`mise run build\`)" >&2
  exit 1
}
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-boot}
export LINUXAPK_DATA=$repo/out/test/boot/data

"$session" stop >/dev/null
"$session" reset >/dev/null
trap '"$session" stop >/dev/null; "$session" reset >/dev/null' EXIT
"$session" wait
t=$SECONDS
"$session" start --headless
until [[ $("$session" exec getprop sys.boot_completed 2>/dev/null) == 1 ]]; do
  "$session" status >/dev/null || {
    echo "FAIL the session stopped while booting (\`LINUXAPK_INSTANCE=$LINUXAPK_INSTANCE mise run session -- log\`)" >&2
    exit 1
  }
  ((SECONDS - t < 300)) || {
    echo "FAIL not booted after 300 s" >&2
    exit 1
  }
  sleep 2
done
echo "ok   booted in $((SECONDS - t)) s"
"$session" selftest
