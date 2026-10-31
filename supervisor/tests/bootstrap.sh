#!/usr/bin/env bash
# The broker starts init as its child under the filter, stays its parent even
# with Yama off, and exits with init's status. No namespaces or session: a
# host shell stands in for init.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
cargo build -q -p innkeeper-broker --bin innkeeper-brk
broker=$repo/out/cargo/debug/innkeeper-brk
# shellcheck disable=SC2016 # expanded by the filtered child
if timeout 5 setpriv --no-new-privs "$broker" --preload libc.so.6 /bin/sh -c '
  test "$(cat /proc/$PPID/comm)" = innkeeper-brk || exit 99
  test "$LD_PRELOAD" = libc.so.6 && test "$INNKEEPER_SHIM_ROLE" = init || exit 98
  exit 42
'; then
  result=0
else
  result=$?
fi
[[ $result == 42 ]] || {
  echo "FAIL ancestor broker / init environment / exit status: $result"
  exit 1
}
echo 'ok   the broker is init'"'"'s parent, preloads only init and keeps its exit status'
# Init killed by a signal exits with 128 + the signal.
if timeout 5 setpriv --no-new-privs "$broker" /bin/sh -c 'kill -TERM $$'; then
  result=0
else
  result=$?
fi
[[ $result == 143 ]] || {
  echo "FAIL init signal status: $result"
  exit 1
}
echo 'ok   broker preserves init signal status'
