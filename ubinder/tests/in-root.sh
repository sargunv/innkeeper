# shellcheck shell=sh
# The Android side of run-binder-tests.sh (mksh): servicemanager, then each
# test binary. Usage: in-root.sh broker|kernel TEST[:GTEST_FILTER]...
mode=$1
shift
export HOME=/data/local/tmp
/apex/com.android.runtime/bin/linkerconfig --target /linkerconfig >/data/local/tmp/linkerconfig.log 2>&1 ||
  {
    echo "linkerconfig failed:"
    cat /data/local/tmp/linkerconfig.log
  } >&2
export LD_PRELOAD=libprops_shim.so

log=/logs/$mode-servicemanager.log
servicemanager >"$log" 2>&1 &
sm=$!
i=0
until timeout 1 service check manager 2>/dev/null | grep -q ': found'; do
  if ! kill -0 "$sm" 2>/dev/null || [ $i -gt 100 ]; then
    if [ "$mode" = kernel ] && grep -q 'Could not become context manager' "$log"; then
      echo "kernel binder oracle unavailable on this host: BINDER_SET_CONTEXT_MGR denied" >&2
      exit 77
    fi
    echo "servicemanager did not start (out/test/binder/$mode-servicemanager.log)" >&2
    exit 1
  fi
  i=$((i + 1))
  sleep 0.05
done

rc=0
for spec in "$@"; do
  t=${spec%%:*}
  filter=
  [ "$t" != "$spec" ] && filter="--gtest_filter=${spec#*:}"
  echo "== $t $filter"
  # An absolute path: binderLibTest re-executes argv[0] for its server.
  # shellcheck disable=SC2086 # TEST_ARGS is a word list
  timeout "$TEST_TIMEOUT" "/data/nativetest64/$t" $filter $TEST_ARGS >"/logs/$mode-$t.log" 2>&1
  r=$?
  grep -E '^\[  (PASSED|FAILED) |^\[==========\] .* ran' "/logs/$mode-$t.log" | grep -v ' listed below' | sort -u
  if [ $r = 0 ]; then echo "== $t: PASS"; else
    echo "== $t: FAIL ($r)"
    rc=1
  fi
done
exit $rc
