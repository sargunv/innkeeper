#!/usr/bin/env bash
# Screenshot the Linux.apk Wayland window (KDE Plasma): activates it through a
# KWin script, then captures only that window with Spectacle. This steals
# focus. Usage: tools/window-shot.sh <out.png> [window caption, default Linux.apk]
# With several sessions open (one window each), LINUXAPK_SHOT_PID=<host pid of
# the composer> picks the window of one of them.
set -euo pipefail
out=${1:?usage: window-shot.sh <out.png> [caption]}
caption=${2:-Linux.apk}
pid=${LINUXAPK_SHOT_PID:-0}
[[ $pid =~ ^[0-9]+$ ]] || pid=0
js=$(mktemp --suffix=.js)
trap 'rm -f "$js"' EXIT
cat >"$js" <<JS
for (const w of workspace.windowList()) {
  if (w.caption === "$caption" && ($pid === 0 || w.pid === $pid)) {
    w.minimized = false;
    workspace.activeWindow = w;
  }
}
JS
name=linuxapk-activate-$$
id=$(gdbus call --session --dest org.kde.KWin --object-path /Scripting \
  --method org.kde.kwin.Scripting.loadScript "$js" "$name" | grep -oE '[0-9]+')
gdbus call --session --dest org.kde.KWin --object-path "/Scripting/Script$id" \
  --method org.kde.kwin.Script.run >/dev/null
sleep 0.5
gdbus call --session --dest org.kde.KWin --object-path /Scripting \
  --method org.kde.kwin.Scripting.unloadScript "$name" >/dev/null
spectacle -b -n -a -o "$(realpath -m "$out")" >/dev/null 2>&1
echo "$out"
