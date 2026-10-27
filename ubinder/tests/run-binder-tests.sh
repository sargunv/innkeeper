#!/usr/bin/env bash
# Differential binder test harness for ubinder.
#
#   ubinder/tests/run-binder-tests.sh [--ubinder] [TEST[:GTEST_FILTER] ...]
#
# Runs AOSP's own binder test binaries (built from android-17.0.0_r1, the same
# release as the prebuilt image, by the image-builder lane) in a rootless
# user+mount+pid namespace built from the image's /system and flattened /apex,
# like hal/tests/run-android.sh, with the image's servicemanager running as the
# context manager (the tests expect one, as on a device). These are the
# differential oracle: the same binaries must pass against kernel binder and
# against ubinder.
#
#   (default)   kernel binder: a private binderfs is mounted at /dev/binderfs
#               (needs the one-time dev SELinux module for set_context_mgr).
#   --ubinder   no binderfs: the session's pivot-exec installs the seccomp
#               filter and forks the Rust broker (linuxapk-ubinder), exactly as
#               `linuxapk-session start --ubinder` does.
#
# Env: UBINDER_TEST_TIMEOUT (seconds per binary, default 300),
#      LINUXAPK_UBINDER_DEBUG=1 (broker tracing, to $OUT/ubinder-tests/logs).
# Logs: $OUT/ubinder-tests/logs/<kernel|ubinder>-<test>.log
#
# Test binaries are expected at $OUT/ubinder-tests/ (copy them from
# ~/aosp/a17/out/target/product/generic_x86_64/data/nativetest64/<name>/<name>).
# Known names: binderDriverInterfaceTest binderLibTest binderSafeInterfaceTest
#              binderThroughputTest.
#
# Host safety (docs/NOTES.md): no /sys, /dev is a tmpfs with a few bound nodes,
# nothing from $HOME or /run/user is mounted. The namespace is its own transient
# systemd user unit with a memory and runtime cap.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
main=$(cd "$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
out=${LINUXAPK_OUT:-${OUT:-$main/out}}
tdir=$out/ubinder-tests

# Build the shim, pivot-exec and the broker helper into $tdir/bin.
build() {
  local ndk=${ANDROID_NDK_HOME:-}
  [[ -n $ndk ]] || ndk=$(printf '%s\n' "$HOME"/Android/Sdk/ndk/* | sort -V | tail -1)
  local tc=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
  local cc=$tc/x86_64-linux-android30-clang
  mkdir -p "$tdir/bin" "$tdir/logs"
  local shim=$repo/hal/tests/android/haltest_shim.c
  [[ $tdir/bin/libhaltest_shim.so -nt $shim ]] ||
    "$cc" -shared -fPIC -O2 -Wall -fvisibility=hidden -o "$tdir/bin/libhaltest_shim.so" "$shim"
  ((ubinder)) || return 0
  local h=$repo/session/helper
  if [[ ! -x $tdir/bin/pivot-exec || -n $(find "$h" -newer "$tdir/bin/pivot-exec") ]]; then
    "$cc" -static -O2 -Wall -Werror -o "$tdir/bin/pivot-exec" "$h/pivot-exec.c" "$h/bpfemu.c"
  fi
  (
    cd "$repo" &&
      env CC_x86_64_linux_android="$cc" AR_x86_64_linux_android="$tc/llvm-ar" \
        CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$cc" \
        RUSTFLAGS="-C link-arg=-Wl,--dynamic-linker=/system/bin/bootstrap/linker64" \
        cargo build -q -p ubinder --release --target x86_64-linux-android --bin linuxapk-ubinder -j8
  )
  cp "$repo/target/x86_64-linux-android/release/linuxapk-ubinder" "$tdir/bin/linuxapk-ubinder"
}

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
    [[ -d $tdir ]] || {
      echo "missing $tdir (copy AOSP binder test binaries there)" >&2
      exit 1
    }
    build
    exec systemd-run --user --quiet --collect --wait --pipe --same-dir \
      --unit="linuxapk-ubindertest-$$" -p KillMode=control-group -p RuntimeMaxSec=3600 \
      -p MemoryHigh=3G -p MemoryMax=4G -E OUT="$out" -E UBINDER="$ubinder" \
      -E UBINDER_TEST_TIMEOUT="${UBINDER_TEST_TIMEOUT:-300}" \
      -E LINUXAPK_UBINDER_DEBUG="${LINUXAPK_UBINDER_DEBUG:-}" -- "$0" --ns "$@"
    ;;
esac

ubinder=${UBINDER:-0}
tests=("$@")
((${#tests[@]})) || tests=(binderDriverInterfaceTest binderLibTest binderSafeInterfaceTest)

mount --make-rprivate /
R=$(mktemp -d "$out/ubinder-test-root.XXXXXX")
trap 'umount -R "$R" 2>/dev/null || true; rmdir "$R" 2>/dev/null || true' EXIT
mount -t tmpfs -o mode=0755 root "$R"
mkdir -p "$R"/{system,system_ext,product,vendor,apex,linkerconfig,dev,proc,data,logs} \
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
ro_bind "$tdir" "$R/tests"
mount --bind "$tdir/logs" "$R/logs"
mount -t tmpfs linkerconfig "$R/linkerconfig"
cp "$tdir/bin/libhaltest_shim.so" "$R/data/local/tmp/"

mount -t tmpfs -o mode=0755 dev "$R/dev"
for n in null zero full random urandom; do
  touch "$R/dev/$n"
  mount --bind "/dev/$n" "$R/dev/$n"
done
mkdir -p "$R/dev/socket"

if ((ubinder)); then
  # Dummy readable nodes for libbinder's access("/dev/binder") gate; real opens
  # are intercepted by the seccomp supervisor and get an injected memfd.
  for d in binder hwbinder vndbinder; do
    : >"$R/dev/$d"
    chmod 0666 "$R/dev/$d"
  done
  cp "$tdir/bin/linuxapk-ubinder" "$R/data/local/tmp/"
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

# The Android-side runner (mksh): servicemanager, then each test binary.
mode=kernel
((ubinder)) && mode=ubinder
cat >"$R/run.sh" <<EOF
export PATH=/system/bin:/tests ANDROID_ROOT=/system ANDROID_DATA=/data
export ANDROID_ART_ROOT=/apex/com.android.art ANDROID_I18N_ROOT=/apex/com.android.i18n
export ANDROID_TZDATA_ROOT=/apex/com.android.tzdata TMPDIR=/data/local/tmp HOME=/data/local/tmp
/apex/com.android.runtime/bin/linkerconfig --target /linkerconfig >/data/local/tmp/lc.log 2>&1 ||
  { echo "linkerconfig failed:"; cat /data/local/tmp/lc.log; } >&2
export LD_PRELOAD=/data/local/tmp/libhaltest_shim.so
servicemanager >/logs/$mode-servicemanager.log 2>&1 &
sleep 0.5
rc=0
for spec in $(printf '%q ' "${tests[@]}"); do
  t=\${spec%%:*}
  filter=
  [ "\$t" != "\$spec" ] && filter="--gtest_filter=\${spec#*:}"
  if [ ! -x /tests/\$t ]; then echo "== \$t: MISSING"; rc=1; continue; fi
  echo "== \$t \$filter =="
  timeout $((${UBINDER_TEST_TIMEOUT:-300})) /tests/\$t \$filter >/logs/$mode-\$t.log 2>&1
  r=\$?
  grep -E '^\[  (PASSED|FAILED) |^\[==========\] .* ran' /logs/$mode-\$t.log | grep -v ' listed below' | sort -u
  if [ \$r = 0 ]; then echo "== \$t: PASS"; else echo "== \$t: FAIL (\$r)"; rc=1; fi
done
exit \$rc
EOF

if ((ubinder)); then
  # pivot-exec pivots into $R, installs the combined seccomp filter in a nested
  # namespace set and forks the broker helper (it reads LINUXAPK_UBINDER).
  dbg=${LINUXAPK_UBINDER_DEBUG:-}
  for v in $(compgen -e); do unset "$v"; done
  export LINUXAPK_UBINDER=/data/local/tmp/linuxapk-ubinder
  [[ -n $dbg ]] && export LINUXAPK_UBINDER_DEBUG=$dbg
  # Broker stderr (tracing) goes to a bounded log: tail keeps the last part.
  exec "$tdir/bin/pivot-exec" "$R" /system/bin/sh /run.sh
fi

cd "$R"
pivot_root . .
umount -l .
cd /
for v in $(compgen -e); do unset "$v"; done
exec /system/bin/sh /run.sh
