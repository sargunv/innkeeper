#!/usr/bin/env bash
# Prototype Flatpak of Linux.apk (issue #25): the staged prefix (out/stage,
# after `mise run build`) as /app, exported to a local repo and installed into
# the current user's Flatpak installation (FLATPAK_USER_DIR picks a private
# one). The Android session runs inside the sandbox
# (docs/session.md#flatpak-mode).
#
#   packaging/flatpak/build.sh [<out-dir>]   default out/flatpak
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
top=$(cd "$here/../.." && pwd)
out=${1:-$top/out/flatpak}
id=dev.linuxapk.Manager
runtime=26.08 # glibc 2.44: the host build links against 2.43
[[ -f $top/out/stage/share/linuxapk/manifest.tsv ]] || {
  echo "build.sh: no out/stage (run \`mise run build\`)" >&2
  exit 1
}
b=$out/build
rm -rf "$b"
flatpak build-init "$b" "$id" "org.freedesktop.Sdk//$runtime" "org.freedesktop.Platform//$runtime"
cp -a "$top/out/stage/." "$b/files/"
flatpak build-finish "$b" --command=linuxapk-manager \
  --share=ipc --socket=wayland --socket=pulseaudio \
  `# The render node and /dev/udmabuf; --device=all would bind the host's` \
  `# /dev over the sandbox's writable one.` \
  --device=dri \
  `# The host's network, as any desktop app has.` \
  --share=network
flatpak build-export "$out/repo" "$b"
flatpak remote-add --user --if-not-exists --no-gpg-verify linuxapk-local "$out/repo"
flatpak install --user -y --noninteractive --reinstall linuxapk-local "$id"
