#!/usr/bin/env bash
# Run a command inside the host-tools container with $HOME mounted at the same path.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
if ! podman image exists linuxapk-hosttools; then
  podman build -q -t linuxapk-hosttools -f "$here/Containerfile.hosttools" "$here" >/dev/null
fi
exec podman run --rm --userns=keep-id --security-opt label=disable \
  -v "$HOME:$HOME" -w "$PWD" linuxapk-hosttools "$@"
