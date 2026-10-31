#!/usr/bin/env bash
# Run a native build with sccache sharing one disk cache (SCCACHE_DIR, set in
# mise.toml) across every checkout of this repository on the machine.
#
#   tools/build/sccache.sh CMD [ARGS...]
#
# sccache keys a C/C++ compilation on the preprocessor output, which carries the
# absolute path of every source and header the compiler read. Listing the
# checkouts in SCCACHE_BASEDIRS strips those prefixes from the key, so a new
# worktree reuses what its siblings already compiled.
#
# Shared entries also need preprocessor cache mode (SCCACHE_DIRECT) off. That
# mode answers a repeat compilation from a lookup keyed on the source path and
# command line, and its hit restores the depfile stored with the shared entry,
# which names the headers of whichever checkout compiled it first: Ninja would
# then track another worktree's files and miss header edits here. With the mode
# off each compilation preprocesses locally and keeps the depfile it wrote.
#
# The server reads both settings once, when it starts, so a new worktree means a
# restart. Stopping the server drains the compilations in flight, so a restart
# is safe during a concurrent build. Inside the build container
# (tools/container/run.sh) the server is the container's own and ends with it.
set -euo pipefail
(($#)) || {
  echo "usage: ${0##*/} CMD [ARGS...]" >&2
  exit 2
}
# Git hooks export these; clear them so `git worktree list` resolves normally.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR

SCCACHE_BASEDIRS=$(git -C "$(dirname "$0")" worktree list --porcelain |
  sed -n 's|^worktree ||p' | sort -u | paste -sd: -)
export SCCACHE_BASEDIRS SCCACHE_DIRECT=false

# With no server running, the stats show this client's settings, and the first
# compilation starts a server with them.
norm() { sed 's/[:,]/\n/g' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//; s|/*$||; /^$/d' | sort -u; }
stats=$(sccache --show-stats 2>/dev/null || true)
running=$(sed -n 's/^Base directories[[:space:]]*//p' <<<"$stats")
direct=$(sed -n 's|^Use direct/preprocessor mode?[[:space:]]*||p' <<<"$stats")
if [[ $direct != no || $(norm <<<"$running") != $(norm <<<"$SCCACHE_BASEDIRS") ]]; then
  sccache --stop-server >/dev/null 2>&1 || true
  sccache --start-server >/dev/null
fi
exec "$@"
