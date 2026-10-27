#!/usr/bin/env bash
# Run a command (default: bash) in the AOSP build container with the AOSP tree mounted.
# Usage: aosp/build-shell.sh [cmd...]   e.g. aosp/build-shell.sh bash -c '. build/envsetup.sh && lunch ... && m'
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
AOSP=${AOSP:-$HOME/aosp/a17}
if ! podman image exists linuxapk-aospbuild; then
  podman build -q -t linuxapk-aospbuild -f "$here/Containerfile.build" "$here" >/dev/null
fi
mkdir -p "$HOME/.cache/linuxapk-ccache"
exec podman run --rm -i $([ -t 0 ] && echo -t) --userns=keep-id --security-opt label=disable \
  --ulimit nofile=65536:65536 \
  -v "$AOSP:$AOSP" -v "$HOME/Projects:$HOME/Projects" -v "$HOME/.cache/linuxapk-ccache:/ccache" \
  -e USE_CCACHE=1 -e CCACHE_DIR=/ccache -e CCACHE_EXEC=/usr/bin/ccache \
  -w "$AOSP" linuxapk-aospbuild "${@:-bash}"
