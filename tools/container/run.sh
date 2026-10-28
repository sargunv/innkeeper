#!/usr/bin/env bash
# Run a command in the build container, with $HOME mounted at the same path and
# the toolchain pin in its environment. Every run is capped (memory, pids), and
# JOBS (at most 12) is the parallelism builds inside should use. The image tag
# is the Containerfile's hash, so editing it rebuilds the image.
#
#   tools/container/run.sh CMD [ARGS...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$(dirname "$here")")
: "${ANDROID_HOME:?not set (run through mise)}"
img=linuxapk-build:$(sha256sum "$here/Containerfile" | cut -c1-12)
if ! podman image exists "$img"; then
  podman build -q -t "$img" -f "$here/Containerfile" "$here" >/dev/null
fi
jobs=$(nproc)
((jobs <= 12)) || jobs=12
exec podman run --rm --userns=keep-id --security-opt label=disable \
  --memory=16g --memory-swap=16g --pids-limit=8192 \
  --env-file "$repo/tools/build/toolchain.env" -e ANDROID_HOME -e HOME -e JOBS="$jobs" \
  -v "$HOME:$HOME" -w "$PWD" "$img" "$@"
