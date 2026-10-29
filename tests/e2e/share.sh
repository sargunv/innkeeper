#!/usr/bin/env bash
# Runs only inside the grouped runner's fresh, isolated desktop and Android session.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
T=$repo/out/test/e2e/$LINUXAPK_INSTANCE
S=$repo/tools/dev/session.sh
s() { "$S" exec "$@"; }
wm() { s am broadcast -n dev.linuxapk.system/.wm.ControlReceiver -a "dev.linuxapk.$1" "${@:2}"; }
log_has() { s logcat -d -s "$1:I" | grep -E "$2" >/dev/null; }
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
chooser_ready() { s dumpsys activity activities | grep 'topResumedActivity=.*OpenWithActivity' >/dev/null; }
choose_share() {
  until_ok chooser_ready
  local display
  display=$(wm LIST | sed -n 's/.*display=\([0-9]*\) package=dev.linuxapk.system .*/\1/p')
  # Disabled section headings are skipped: first Open target, then Share probe.
  s input -d "$display" keyevent KEYCODE_DPAD_DOWN KEYCODE_DPAD_DOWN KEYCODE_ENTER
}
wm CLOSE --es package dev.linuxapk.probes >/dev/null || true
printf 'host share\000\377\n' >"$T/share.md"
printf 'second host attachment\n' >"$T/share.txt"
first=$(sha256sum "$T/share.md" | cut -d ' ' -f1)
second=$(sha256sum "$T/share.txt" | cut -d ' ' -f1)
s logcat -c
"$repo/tools/dev/linuxapk.sh" open "$T/share.md"
until_ok log_has LinuxApkOpenWith 'share action=android.intent.action.SEND files=1 apps=dev.linuxapk.probes$'
until_ok chooser_ready
"$repo/tools/dev/window-shot.sh" "$T/share-chooser.png" 'Linux.apk System'
choose_share
until_ok log_has LinuxApkShareProbe 'action=android.intent.action.SEND type=text/markdown count=1 read=true clip=true'
until_ok log_has LinuxApkShareProbe "item=0 bytes=13 sha256=$first uri=content://.* write=false"
s logcat -d -s LinuxApkOpenWith:I LinuxApkShareProbe:I >"$T/share-single.txt"
wm CLOSE --es package dev.linuxapk.probes >/dev/null
s logcat -c
"$repo/tools/dev/linuxapk.sh" open "$T/share.md" "$T/share.txt"
# MarkdownShareOnly must be excluded from the mixed batch, even though it matches text/*.
until_ok log_has LinuxApkOpenWith 'share action=android.intent.action.SEND_MULTIPLE files=2 apps=dev.linuxapk.probes$'
choose_share
until_ok log_has LinuxApkShareProbe 'action=android.intent.action.SEND_MULTIPLE type=text/\* count=2 read=true clip=true'
until_ok log_has LinuxApkShareProbe "item=0 bytes=13 sha256=$first uri=content://.* write=false"
until_ok log_has LinuxApkShareProbe "item=1 bytes=23 sha256=$second uri=content://.* write=false"
s logcat -d -s LinuxApkOpenWith:I LinuxApkShareProbe:I >"$T/share-multiple.txt"
wm CLOSE --es package dev.linuxapk.probes >/dev/null
printf 'ok   host SEND and SEND_MULTIPLE: matching bytes, MIME, ClipData and read-only grants\n'
