#!/usr/bin/env bash
# Differential binder test harness: AOSP's own binder tests, first against the
# broker (binder in userspace), then against kernel binder as the oracle.
#
#   supervisor/tests/binder/run.sh [TEST[:GTEST_FILTER] ...]
#
# The tests are android-17.0.0_r1 builds (the image's release) of
# binderDriverInterfaceTest, binderLibTest, binderSafeInterfaceTest and
# binderThroughputTest, read from $XDG_DATA_HOME/linuxapk/test-bin/binder and
# checked against test-bin.sha256. Android's CI doesn't publish them: the
# image's branch (aosp-android-latest-release) builds only `droid dist`, whose
# artifacts have no test suites, and binderThroughputTest is in no suite at
# all. Build them in an AOSP checkout of that release (`m <name>`) and copy
# them from out/target/product/<device>/data/nativetest64/<name>/<name>.
# They run with the image's servicemanager as the context manager, as on a
# device, in a root from tests/bionic/lib.sh:
#
#   broker  the session's pivot-exec installs the seccomp filter and forks the
#           broker, as `linuxapk-session start` does
#   kernel  a private binderfs. If the host forbids mounting it or becoming
#           its context manager (SELinux), this prints "kernel binder oracle
#           unavailable on this host: <reason>" and exits 77 after the broker
#           run.
#
# By default binderLibTest skips the tests that fail for environmental
# reasons: binder netlink reports (only kernel binder sends them, and the
# oracle can't receive them), RT scheduling, setuid and the SELinux access
# check. The freezer tests run in the unit's delegated cgroup; on kernel
# binder they skip, since BINDER_FREEZE takes host pids and the tests run in
# a pid namespace.
#
# Env: BINDER_TEST_TIMEOUT (seconds per binary, default 300),
#      BINDER_TEST_ARGS (extra arguments for every binary, e.g. for
#      binderThroughputTest: "-w 2 -i 20000 -p"),
#      LINUXAPK_BROKER_DEBUG=1 (broker tracing, to stderr).
# Logs: out/test/binder/<broker|kernel>-<test>.log
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=../../../tests/bionic/lib.sh
. "$here/../../../tests/bionic/lib.sh"
logs=$repo/out/test/binder

if [[ ${1:-} == --inner ]]; then
  shift
else
  testbin=${XDG_DATA_HOME:-$HOME/.local/share}/linuxapk/test-bin/binder
  (cd "$testbin" && sha256sum --quiet -c "$here/test-bin.sha256") || {
    echo "$testbin does not match supervisor/tests/binder/test-bin.sha256; refusing to run" >&2
    echo "(the binaries come from an AOSP build; see the top of $0)" >&2
    exit 1
  }
  tests=("$@")
  ((${#tests[@]})) || tests=(
    binderDriverInterfaceTest
    binderLibTest:-BinderLibTest.BinderNetlink*:BinderLibTest.CheckServiceAccessNotOk:BinderLibTest.GetCallingUidWithSetuid:BinderLibTest.InheritRt:BinderLibTest.SchedPolicySet
    binderSafeInterfaceTest
    binderThroughputTest
  )
  bionic_prepare
  mkdir -p "$logs"
  status=()
  for mode in broker kernel; do
    echo "=== $mode binder"
    rc=0
    bionic_unit "linuxapk-bindertest-$mode" 3600 -E BINDER_TESTS="$testbin" \
      -E BINDER_TEST_TIMEOUT="${BINDER_TEST_TIMEOUT:-300}" -E BINDER_TEST_ARGS="${BINDER_TEST_ARGS:-}" \
      -E LINUXAPK_BROKER_DEBUG="${LINUXAPK_BROKER_DEBUG:-}" -- "$mode" "${tests[@]}" || rc=$?
    status+=("$rc")
  done
  ((status[0])) && exit "${status[0]}"
  exit "${status[1]}"
fi

img=$BIONIC_IMG apex=$BIONIC_APEX
mode=$1
shift
R=$scratch/root
bionic_root "$R" "$img/vendor"
# The unit's own cgroup, for the freezer tests: libprocessgroup's
# FreezerState is cgroup.freeze there, and its process groups live under
# system/ and apps/ (init makes those on a device).
mkdir -p "$R/sys/fs/cgroup"
mount -t cgroup2 -o nosuid,nodev,noexec cgroup2 "$R/sys/fs/cgroup"
mkdir -p "$R/sys/fs/cgroup/system" "$R/sys/fs/cgroup/apps"
mkdir -p "$R/data/nativetest64" "$R/logs"
ro_bind "$BINDER_TESTS" "$R/data/nativetest64"
mount --bind "$logs" "$R/logs"
cp "$here/in-root.sh" "$R/in-root.sh"
timeout=$BINDER_TEST_TIMEOUT args=$BINDER_TEST_ARGS debug=$LINUXAPK_BROKER_DEBUG

if [[ $mode == broker ]]; then
  bionic_broker_nodes "$R"
  cp "$broker" "$R/data/local/tmp/"
  bionic_env
  export LINUXAPK_BROKER=/data/local/tmp/linuxapk-broker
  [[ -z $debug ]] || export LINUXAPK_BROKER_DEBUG=$debug
  export TEST_TIMEOUT=$timeout TEST_ARGS=$args
  exec "$pivot_exec" "$R" /system/bin/sh /in-root.sh "$mode" "$@"
fi

mkdir -p "$R/dev/binderfs"
if ! err=$(mount -t binder binder "$R/dev/binderfs" 2>&1); then
  err=${err%%$'\n'*}
  echo "kernel binder oracle unavailable on this host: cannot mount binderfs: ${err##*: }" >&2
  exit 77
fi
for b in binder hwbinder vndbinder; do ln -s "binderfs/$b" "$R/dev/$b"; done
cd "$R"
pivot_root . .
umount -l .
cd /
bionic_env
export TEST_TIMEOUT=$timeout TEST_ARGS=$args
exec /system/bin/sh /in-root.sh "$mode" "$@"
