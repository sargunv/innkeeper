#!/usr/bin/env bash
# The session's seccomp filter (pivot-exec/bpfemu.c) traps exactly the syscalls
# each mode needs: bpf and friends, openat and binder ioctls always; the
# credential syscalls, SO_PEERCRED, lstat-shaped newfstatat and W_OK access
# checks in single-uid mode (docs/sandbox.md); and nothing hot besides.
# Builds into out/test/supervisor/seccomp-filter.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/seccomp-filter
mkdir -p "$out"
cc -O1 -Wall -Werror -o "$out/t" "$here/seccomp-filter.c" "$here/../pivot-exec/bpfemu.c" -lpthread
fail=0
# mode single-uid, then the probes expected to be notified.
check() {
  local mode=$1 s=$2 want=$3 got
  got=$("$out/t" "$s" | awk '$NF == 1 { $NF = ""; sub(/ $/, ""); print }' | paste -sd, -)
  if [[ $got == "$want" ]]; then echo "ok   $mode: $got"; else
    echo "FAIL $mode: notified [$got], want [$want]"
    fail=1
  fi
}
check "subuid" 0 "bpf,openat,ioctl binder"
check "single-uid" 1 "bpf,openat,ioctl binder,getuid,getgroups,setresuid,setgroups,fchownat,getsockopt SO_PEERCRED,lstat,access W_OK"
exit $fail
