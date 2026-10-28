#!/usr/bin/env bash
# Boot a fresh headless test instance (LINUXAPK_INSTANCE=boot, its /data under
# out/test/boot), wait for Android to finish booting, run the host-safety
# selftest, and stop it again.
#   tests/boot.sh [--single-uid]
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
[[ -x $session ]] || {
  echo "no $session (run \`mise run build\`)" >&2
  exit 1
}
mode=subuid start=(--headless)
if [[ ${1:-} == --single-uid ]]; then
  mode=single-uid
  start+=(--single-uid)
fi
export LINUXAPK_INSTANCE=boot LINUXAPK_DATA=$repo/out/test/boot/data-$mode

paths=$("$session" image)
img=$(sed -n 's/^img=//p' <<<"$paths") apex=$(sed -n 's/^apex=//p' <<<"$paths")
[[ -d $img/system/system && -f $apex/apex-info-list.xml ]] || {
  echo "image not prepared (run \`mise run session -- prepare\`)" >&2
  exit 1
}
avail=$(awk '/^MemAvailable:/ {print int($2 / 1048576)}' /proc/meminfo)
((avail >= 12)) || {
  echo "only ${avail} GB available; not starting a session" >&2
  exit 1
}

"$session" stop >/dev/null
# A subuid session's /data belongs to subordinate ids.
podman unshare rm -rf "$LINUXAPK_DATA"
trap '"$session" stop >/dev/null' EXIT
"$session" start "${start[@]}"
t=$SECONDS
until [[ $("$session" exec getprop sys.boot_completed 2>/dev/null) == 1 ]]; do
  "$session" status >/dev/null || {
    echo "FAIL the session stopped while booting (\`LINUXAPK_INSTANCE=boot mise run session -- log\`)" >&2
    exit 1
  }
  ((SECONDS - t < 300)) || {
    echo "FAIL not booted after 300 s" >&2
    exit 1
  }
  sleep 2
done
echo "ok   booted ($mode) in $((SECONDS - t)) s"
"$session" selftest
