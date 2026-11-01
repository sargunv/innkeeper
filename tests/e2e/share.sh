#!/usr/bin/env bash
# The grouped e2e runner runs this inside its fresh, isolated desktop and
# Android session.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
T=$repo/out/test/e2e/$INNKEEPER_PROFILE
S=$repo/tools/dev/session.sh
s() { "$S" exec "$@"; }
wm() { /usr/bin/python3 "$repo/tests/e2e/wm.py" "$@"; }
until_ok() {
  local end=$((SECONDS + 10))
  until "$@"; do
    ((SECONDS < end)) || {
      echo "FAIL waiting for $*" >&2
      return 1
    }
    sleep 0.1
  done
}
chooser_ready() {
  wm LIST >"$T/share-windows.txt"
  display=$(sed -n 's/.*display=\([0-9]*\) package=dev.sargunv.innkeeper.system .*window=yes.*/\1/p' "$T/share-windows.txt")
  [[ -n $display ]]
}
choose_share() {
  until_ok chooser_ready
  # Focus skips the disabled section headings, so the first DPAD_DOWN reaches
  # the Open target and the second reaches the Share probe.
  s input -d "$display" keyevent KEYCODE_DPAD_DOWN KEYCODE_DPAD_DOWN KEYCODE_ENTER
}
received() {
  s logcat -d -s InnkeeperOpenWith:I InnkeeperShareProbe:I >"$T/share-received.txt"
  grep -F "action=android.intent.action.SEND_MULTIPLE type=text/* count=2 read=true clip=true" "$T/share-received.txt" >/dev/null &&
    grep -E "item=0 bytes=13 sha256=$first uri=content://.* write=false" "$T/share-received.txt" >/dev/null &&
    grep -E "item=1 bytes=23 sha256=$second uri=content://.* write=false" "$T/share-received.txt" >/dev/null
}
printf 'host share\000\377\n' >"$T/share.md"
printf 'second host attachment\n' >"$T/share.txt"
first=$(sha256sum "$T/share.md" | cut -d ' ' -f1)
second=$(sha256sum "$T/share.txt" | cut -d ' ' -f1)
s logcat -c
"$repo/tools/dev/innkeeper.sh" open "$T/share.md" "$T/share.txt"
# MarkdownShareOnly matches text/*, but it must not be offered for the mixed
# batch.
# shellcheck disable=SC2329 # invoked through until_ok
share_targets() {
  s logcat -d -s InnkeeperOpenWith:I | grep -E 'share action=android.intent.action.SEND_MULTIPLE files=2 apps=dev.sargunv.innkeeper.probes$' >/dev/null
}
until_ok share_targets
choose_share
until_ok received
wm CLOSE --es package dev.sargunv.innkeeper.probes >/dev/null
printf 'ok   host multi-file share: matching bytes, MIME, ClipData and read-only grants\n'
