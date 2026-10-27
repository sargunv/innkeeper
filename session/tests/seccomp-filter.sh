#!/usr/bin/env bash
# The session's seccomp filter (helper/bpfemu.c) traps exactly the syscalls
# each mode needs: bpf and friends always, openat and binder ioctls with
# ubinder, the credential syscalls, SO_PEERCRED, lstat-shaped newfstatat
# and W_OK access checks
# in single-uid mode (docs/sandbox.md), and nothing hot besides.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cc -O1 -Wall -Werror -o "$tmp/t" "$here/seccomp-filter.c" "$here/../helper/bpfemu.c" -lpthread
fail=0
# mode binder single-uid, then the probes expected to be notified.
check() {
  local mode=$1 b=$2 s=$3 want=$4 got
  got=$("$tmp/t" "$b" "$s" | awk '$NF == 1 { $NF = ""; sub(/ $/, ""); print }' | paste -sd, -)
  if [[ $got == "$want" ]]; then echo "ok   $mode: $got"; else
    echo "FAIL $mode: notified [$got], want [$want]"
    fail=1
  fi
}
check "kernel binder" 0 0 "bpf"
check "ubinder" 1 0 "bpf,openat,ioctl binder"
check "single-uid" 1 1 "bpf,openat,ioctl binder,getuid,getgroups,setresuid,setgroups,fchownat,getsockopt SO_PEERCRED,lstat,access W_OK"
exit $fail
