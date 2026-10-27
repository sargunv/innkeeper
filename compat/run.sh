#!/usr/bin/env bash
# Compat harness: builds and runs compat/ (Rust) against the live session.
#   compat/run.sh [run] [--window] [--only PKG,...] [--accept] [--no-finish]
#   compat/run.sh fetch | report
# Without a running session it starts a headless one; at the end it leaves a
# session running with its window shown (--no-finish to skip).
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
export LINUXAPK_REPO=$repo
# A lane worktree has no extracted image; drive the main checkout's session.
if [[ -z ${LINUXAPK_SESSION:-} && ! -d $repo/out/img/system ]]; then
  main=$(dirname "$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)")
  export LINUXAPK_SESSION=$main/session/linuxapk-session
fi
cargo build -q -j 8 --release --manifest-path "$repo/compat/Cargo.toml"
exec "$repo/target/release/linuxapk-compat" "$@"
