# shellcheck shell=sh
# Stands in for Android init in tests/bionic/run.sh's root (mksh, run by
# pivot-exec): generate /linkerconfig, start servicemanager, our allocator and
# our composer, then run the test command with $TIMEOUT.

/apex/com.android.runtime/bin/linkerconfig --target /linkerconfig >/data/local/tmp/linkerconfig.log 2>&1 ||
  {
    echo "linkerconfig failed:"
    cat /data/local/tmp/linkerconfig.log
  } >&2
export LD_PRELOAD=libprops_shim.so

# Wait until servicemanager lists NAME.
wait_service() {
  i=0
  until timeout 1 service check "$1" 2>/dev/null | grep -q ': found'; do
    i=$((i + 1))
    if [ $i -gt 100 ]; then
      echo "init.sh: $1 did not register" >&2
      exit 1
    fi
    sleep 0.05
  done
}

servicemanager &
wait_service manager
/vendor/bin/hw/android.hardware.graphics.allocator-service.linuxapk &
/vendor/bin/hw/android.hardware.graphics.composer3-service.linuxapk &
wait_service android.hardware.graphics.allocator.IAllocator/default
wait_service android.hardware.graphics.composer3.IComposer/default

timeout "$TIMEOUT" "$@"
rc=$?
[ $rc = 124 ] && echo "tests/bionic/run.sh: $1 timed out after ${TIMEOUT}s" >&2
exit $rc
