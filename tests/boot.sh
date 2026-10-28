#!/usr/bin/env bash
# Boot a fresh headless test instance (LINUXAPK_INSTANCE, default boot; its
# /data under out/test/boot), wait for Android to finish booting, run the
# host-safety selftest, and stop it again. The uid mode is the host's: subuid when it has
# a subordinate id range.
#   tests/boot.sh [--no-subids]
# --no-subids hides the host's ranges (empty /etc/subuid and /etc/subgid
# stand-ins), so the session runs in single mode.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
[[ -x $session ]] || {
  echo "no $session (run \`mise run build\`)" >&2
  exit 1
}
mode=subuid
if [[ ${1:-} == --no-subids ]]; then
  mode=single
  export LINUXAPK_SUBUID_FILE=/dev/null LINUXAPK_SUBGID_FILE=/dev/null
fi
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-boot}
export LINUXAPK_DATA=$repo/out/test/boot/data-$mode

avail=$(awk '/^MemAvailable:/ {print int($2 / 1048576)}' /proc/meminfo)
((avail >= 12)) || {
  echo "only ${avail} GB available; not starting a session" >&2
  exit 1
}

fail=0
check() { # name expected actual
  if [[ $2 == "$3" ]]; then echo "ok   $1"; else
    echo "FAIL $1: expected '$2', got '$3'"
    fail=1
  fi
}
uid_mode() { "$session" status | sed -n 's/^uid-mode=//p'; }

"$session" stop >/dev/null
"$session" reset >/dev/null
trap '"$session" stop >/dev/null' EXIT
"$session" start --headless
t=$SECONDS
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
echo "ok   booted ($mode) in $((SECONDS - t)) s"
check "uid mode" "$mode" "$(uid_mode)"
"$session" selftest || fail=1
"$session" stop >/dev/null
# The /data it made keeps its mode, whatever ranges the host shows now.
check "uid mode sticks to /data without ranges" "$mode" \
  "$(LINUXAPK_SUBUID_FILE=/dev/null LINUXAPK_SUBGID_FILE=/dev/null uid_mode)"
exit $fail
