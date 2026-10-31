#!/usr/bin/env bash
# The real bootstrap must become Android's parent broker, even with Yama off.
# No namespaces or session: a host shell stands in for init under the filter.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=$repo/out/test/supervisor/bootstrap
mkdir -p "$out"
cargo build -q -p innkeeper-broker --bin innkeeper-brk
cc -O1 -Wall -Werror -o "$out/pivot-exec" "$repo/supervisor/pivot-exec/pivot-exec.c"
export INNKEEPER_BROKER=$repo/out/cargo/debug/innkeeper-brk
# shellcheck disable=SC2016 # expanded by the filtered child
if timeout 5 setpriv --no-new-privs "$out/pivot-exec" / /bin/sh -c '
  test "$(cat /proc/$PPID/comm)" = innkeeper-brk || exit 99
  exit 42
'; then
  result=0
else
  result=$?
fi
[[ $result == 42 ]] || {
  echo "FAIL ancestor broker / init exit status: $result"
  exit 1
}
echo 'ok   pivot-exec becomes the parent broker and preserves init exit status'
# A signal exit has the same result as the old bootstrap reaper.
if timeout 5 setpriv --no-new-privs "$out/pivot-exec" / /bin/sh -c 'kill -TERM $$'; then
  result=0
else
  result=$?
fi
[[ $result == 143 ]] || {
  echo "FAIL init signal status: $result"
  exit 1
}
echo 'ok   broker preserves init signal status'
