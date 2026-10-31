# shellcheck shell=sh
# Runs inside the OpenHarmony session with OH's own sh (ohos/run.py --probe),
# once samgr has set bootevent.samgr.ready: init started its first services
# under the supervisor, and they are still running.
for p in samgr hilogd hdf_devmgr appspawn; do
  if ! pidof "$p" >/dev/null; then
    echo "not running: $p"
    ps -A -o PID,NAME
    exit 1
  fi
done
echo "running: samgr hilogd hdf_devmgr appspawn"
