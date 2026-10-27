#!/usr/bin/env bash
# Differential binder test harness for ubinder.
#
#   ubinder/tests/run-binder-tests.sh [--ubinder] [TEST ...]
#
# Runs AOSP's own binder test binaries (built from android-17.0.0_r1, the same
# release as the prebuilt image, by the image-builder lane) in a rootless
# user+mount+pid namespace built from the image's /system and flattened /apex,
# exactly like hal/tests/run-android.sh. These are the differential oracle:
# the same binaries must pass against kernel binder and against ubinder.
#
#   (default)   kernel binder: a private binderfs is mounted at /dev/binderfs
#               (needs the one-time dev SELinux module for set_context_mgr).
#   --ubinder   no binderfs; the session's seccomp supervisor serves binder in
#               userspace. Stubbed until the sys.rs binding lands (see
#               docs/ubinder.md "Remaining work"); prints SKIP for now.
#
# Test binaries are expected at $OUT/ubinder-tests/ (copy them from
# ~/aosp/a17/out/target/product/generic_x86_64/data/nativetest64/<name>/<name>).
# Known names: binderDriverInterfaceTest binderLibTest binderSafeInterfaceTest
#              binderThroughputTest.
#
# Host safety (docs/NOTES.md): no /sys, /dev is a tmpfs with a few bound nodes,
# nothing from $HOME or /run/user is mounted. The namespace is its own transient
# systemd user unit with a runtime cap.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
main=$(cd "$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
out=${LINUXAPK_OUT:-${OUT:-$main/out}}

ubinder=0
case ${1:-} in
  --inner)
    shift
    ;;
  --ns)
    shift
    exec unshare --user --map-root-user --mount --pid --fork --kill-child -- "$0" --inner "$@"
    ;;
  *)
    [[ ${1:-} == --ubinder ]] && {
      ubinder=1
      shift
    }
    [[ -d $out/root/apex/com.android.runtime ]] || {
      echo "missing $out/root/apex (run tools/flatten-apex.sh)" >&2
      exit 1
    }
    [[ -d $out/ubinder-tests ]] || {
      echo "missing $out/ubinder-tests (copy AOSP binder test binaries there)" >&2
      exit 1
    }
    exec systemd-run --user --quiet --collect --wait --pipe --same-dir \
      --unit="linuxapk-ubindertest-$$" -p KillMode=control-group -p RuntimeMaxSec=300 \
      -p MemoryMax=2G -E OUT="$out" -E UBINDER="$ubinder" -- "$0" --ns "$@"
    ;;
esac

ubinder=${UBINDER:-0}
tests=("$@")
((${#tests[@]})) || tests=(binderDriverInterfaceTest binderLibTest binderSafeInterfaceTest)

mount --make-rprivate /
R=$(mktemp -d "$out/ubinder-test-root.XXXXXX")
trap 'umount -R "$R" 2>/dev/null || true; rmdir "$R" 2>/dev/null || true' EXIT
mount -t tmpfs -o mode=0755 root "$R"
mkdir -p "$R"/{system,system_ext,product,vendor,apex,linkerconfig,dev,proc,data} \
  "$R"/data/local/tmp "$R/tests"

ro_bind() {
  mount --bind "$1" "$2"
  mount -o remount,bind,ro "$2"
}
ro_bind "$out/img/system/system" "$R/system"
ro_bind "$out/img/system_ext" "$R/system_ext"
ro_bind "$out/img/product" "$R/product"
ro_bind "$out/img/vendor" "$R/vendor"
ro_bind "$out/root/apex" "$R/apex"
mount --bind "$out/ubinder-tests" "$R/tests"
mount -t tmpfs linkerconfig "$R/linkerconfig"

mount -t tmpfs -o mode=0755 dev "$R/dev"
for n in null zero full random urandom; do
  touch "$R/dev/$n"
  mount --bind "/dev/$n" "$R/dev/$n"
done
mkdir -p "$R/dev/socket"

if ((ubinder)); then
  # ubinder path: the seccomp supervisor injects the binder fds. Not yet wired
  # (docs/ubinder.md). Mark clearly so CI records the gap rather than a pass.
  echo "ubinder backend not yet wired (needs sys.rs seccomp binding); SKIP" >&2
  exit 77 # automake convention: skipped
else
  # Kernel binder: a private binderfs instance for this namespace.
  mkdir -p "$R/dev/binderfs"
  if ! mount -t binder binder "$R/dev/binderfs" 2>/dev/null; then
    echo "could not mount binderfs (kernel CONFIG_ANDROID_BINDERFS?); SKIP" >&2
    exit 77
  fi
  for b in binder hwbinder vndbinder; do ln -sf "binderfs/$b" "$R/dev/$b"; done
fi

mount -t proc proc "$R/proc"
for e in "$out/img/system"/*; do
  n=$(basename "$e")
  [[ -L $e && ! -e $R/$n ]] && cp -P "$e" "$R/$n"
done

cd "$R"
pivot_root . .
umount -l .
cd /

for v in $(compgen -e); do unset "$v"; done
export PATH=/system/bin:/tests ANDROID_ROOT=/system ANDROID_DATA=/data \
  ANDROID_ART_ROOT=/apex/com.android.art ANDROID_I18N_ROOT=/apex/com.android.i18n \
  ANDROID_TZDATA_ROOT=/apex/com.android.tzdata TMPDIR=/data/local/tmp \
  HOME=/data/local/tmp

/apex/com.android.runtime/bin/linkerconfig --target /linkerconfig >/data/local/tmp/lc.log 2>&1 ||
  {
    echo "linkerconfig failed:"
    cat /data/local/tmp/lc.log
  } >&2

rc=0
for t in "${tests[@]}"; do
  bin=/tests/$t
  [[ -x $bin ]] || {
    echo "== $t: MISSING"
    continue
  }
  echo "== $t =="
  # binderDriverInterfaceTest / binderLibTest spin up their own service and
  # become context manager; a gtest timeout keeps a hang from wedging the unit.
  if timeout 120 "$bin"; then
    echo "== $t: PASS"
  else
    echo "== $t: FAIL ($?)"
    rc=1
  fi
done
exit $rc
