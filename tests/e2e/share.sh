#!/usr/bin/env bash
# Runs only inside the grouped runner's fresh, isolated desktop and Android session.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
T=$repo/out/test/e2e/$LINUXAPK_INSTANCE
S=$repo/tools/dev/session.sh
s() { "$S" exec "$@"; }
wm() { s am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a "dev.linuxapk.$1" "${@:2}"; }
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
  display=$(sed -n 's/.*display=\([0-9]*\) package=dev.linuxapk.system .*window=yes.*/\1/p' "$T/share-windows.txt")
  [[ -n $display ]]
}
choose_share() {
  until_ok chooser_ready
  # Disabled section headings are skipped: first Open target, then Share probe.
  s input -d "$display" keyevent KEYCODE_DPAD_DOWN KEYCODE_DPAD_DOWN KEYCODE_ENTER
}
received() {
  local kind=$1 action=$2 type=$3 count=$4
  s logcat -d -s LinuxApkOpenWith:I LinuxApkShareProbe:I >"$T/share-$kind.txt"
  grep -F "action=android.intent.action.$action type=$type count=$count read=true clip=true" "$T/share-$kind.txt" >/dev/null &&
    grep -E "item=0 bytes=13 sha256=$first uri=content://.* write=false" "$T/share-$kind.txt" >/dev/null &&
    { [[ $count == 1 ]] || grep -E "item=1 bytes=23 sha256=$second uri=content://.* write=false" "$T/share-$kind.txt" >/dev/null; }
}
share_targets() {
  s logcat -d -s LinuxApkOpenWith:I | grep -E "share action=android.intent.action.$1 files=$2 apps=dev.linuxapk.probes$" >/dev/null
}
printf 'host share\000\377\n' >"$T/share.md"
printf 'second host attachment\n' >"$T/share.txt"
first=$(sha256sum "$T/share.md" | cut -d ' ' -f1)
second=$(sha256sum "$T/share.txt" | cut -d ' ' -f1)
s logcat -c
"$repo/tools/dev/linuxapk.sh" open "$T/share.md"
until_ok share_targets SEND 1
# Screenshot proof is opt-in; normal regression runs exercise the same chooser
# through real input without starting Spectacle or its fixed activation delay.
if [[ ${LINUXAPK_SHARE_SCREENSHOT:-} == 1 ]]; then
  until_ok chooser_ready
  "$repo/tools/dev/window-shot.sh" "$T/share-chooser.png" 'Linux.apk System'
fi
choose_share
until_ok received single SEND text/markdown 1
# Keep the receiver window: the second share must deliver a new intent there.
s logcat -c
"$repo/tools/dev/linuxapk.sh" open "$T/share.md" "$T/share.txt"
# MarkdownShareOnly must be excluded from the mixed batch, even though it matches text/*.
until_ok share_targets SEND_MULTIPLE 2
choose_share
until_ok received multiple SEND_MULTIPLE 'text/*' 2
wm CLOSE --es package dev.linuxapk.probes >/dev/null
printf 'ok   host SEND and SEND_MULTIPLE: matching bytes, MIME, ClipData and read-only grants\n'
