#!/usr/bin/env bash
# Run a command in the build container, with $HOME mounted at the same path and
# the toolchain pin in its environment. Every run is capped (memory, pids), and
# JOBS (at most 12) is the parallelism builds inside should use. It runs at a
# low CPU priority (nice 10) so the desktop stays responsive, without a CPU
# cap. The pinned sccache is mounted in and the command runs under
# tools/build/sccache.sh, against the host's shared cache directory. The image
# tag is the Containerfile's hash, so editing it rebuilds the image.
#
#   tools/container/run.sh CMD [ARGS...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$(dirname "$here")")
: "${ANDROID_HOME:?not set (run through mise)}"
: "${SCCACHE_DIR:?not set (run through mise)}"
sccache=$(mise which sccache)
img=innkeeper-build:$(sha256sum "$here/Containerfile" | cut -c1-12)
if ! podman image exists "$img"; then
  podman build -q -t "$img" -f "$here/Containerfile" "$here" >/dev/null
fi
jobs=$(nproc)
((jobs <= 12)) || jobs=12
mkdir -p "$SCCACHE_DIR"
exec nice -n 10 podman run --rm --userns=keep-id --security-opt label=disable \
  --memory=16g --memory-swap=16g --pids-limit=8192 \
  --env-file "$repo/tools/build/toolchain.env" -e ANDROID_HOME -e HOME -e JOBS="$jobs" \
  -e SCCACHE_DIR -e SCCACHE_SERVER_PORT -e SCCACHE_CACHE_SIZE -v "$sccache:/usr/local/bin/sccache:ro" \
  -v "$HOME:$HOME" -w "$PWD" "$img" "$repo/tools/build/sccache.sh" "$@"
