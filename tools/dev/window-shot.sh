#!/usr/bin/env bash
# Screenshot the Linux.apk Wayland window (KDE Plasma): activates it through a
# KWin script, then captures only that window with Spectacle. This steals
# focus. Usage: tools/dev/window-shot.sh <out.png> [window caption, default Linux.apk]
set -euo pipefail
out=${1:?usage: window-shot.sh <out.png> [caption]}
caption=${2:-Linux.apk}
js=$(mktemp --suffix=.js)
trap 'rm -f "$js"' EXIT
cat >"$js" <<JS
for (const w of workspace.windowList()) {
  if (w.caption === "$caption") {
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
