#!/usr/bin/env bash
# A windowed compat run always gets a disposable desktop.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-compat}
if [[ ${LINUXAPK_NESTED:-} != 1 ]]; then
  for arg in "$@"; do
    if [[ $arg == --window ]]; then exec "$repo/tests/nested.sh" "$0" "$@"; fi
  done
fi
cd "$repo"
exec cargo run -q --release -p linuxapk-compat -- "$@"
