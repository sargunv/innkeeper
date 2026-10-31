#!/usr/bin/env bash
# The session's seccomp filter (broker/src/filter.rs) traps exactly the syscalls
# each mode needs: bpf and friends, openat and binder ioctls always; the
# credential syscalls, SO_PEERCRED, lstat-shaped newfstatat and W_OK access
# checks in single-uid mode (supervisor/README.md); readlink, getcwd, fstat,
# mkdir, chmod, SO_MARK, capget, capset and the capability prctls in
# Flatpak mode; open(2), access-token and ashmem ioctls for an OpenHarmony
# guest only;
# and nothing hot besides.
# Builds into out/test/supervisor/seccomp-filter.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/seccomp-filter
mkdir -p "$out"
cargo build -q -p linuxapk-broker --example supervisor-probes
cc -O1 -Wall -Werror -o "$out/t" "$here/seccomp-filter.c" "$here/../../out/cargo/debug/examples/libsupervisor_probes.a" -lpthread -ldl -lm
fail=0
# Name, harness flag, the probes expected to be notified, then environment.
check() {
  local mode=$1 harness=$2 want=$3 got
  got=$(env "${@:4}" "$out/t" "$harness" | awk '$NF == 1 { $NF = ""; sub(/ $/, ""); print }' | paste -sd, -)
  if [[ $got == "$want" ]]; then echo "ok   $mode: $got"; else
    echo "FAIL $mode: notified [$got], want [$want]"
    fail=1
  fi
}
check "bootless harness" 1 "bpf,openat,ioctl binder"
single="bpf,openat,ioctl binder,getuid,getgroups,setresuid,setgroups,fchownat,getsockopt SO_PEERCRED,lstat,access W_OK"
flat="fstat,readlinkat,getcwd,setsockopt SO_MARK,capget,capset,mkdirat,fchmodat,prctl PR_CAPBSET_DROP"
check "flatpak" 0 "$single,$flat"
check "flatpak, OpenHarmony" 0 "bpf,openat,ioctl binder,open,ioctl access token,ioctl ashmem,${single#bpf,openat,ioctl binder,},$flat" LINUXAPK_GUEST=ohos
exit $fail
