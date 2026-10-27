#!/usr/bin/env bash
# Run a command inside the gpu build container with $HOME mounted at the same path.
# The image tag is the Containerfile hash, so edits rebuild it automatically.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
img=linuxapk-gpubuild:$(sha256sum "$here/Containerfile" | cut -c1-12)
if ! podman image exists "$img"; then
  podman build -q -t "$img" -f "$here/Containerfile" "$here" >/dev/null
fi
exec podman run --rm --userns=keep-id --security-opt label=disable \
  -v "$HOME:$HOME" -w "$PWD" -e HOME="$HOME" -e LINUXAPK_OUT "$img" "$@"
