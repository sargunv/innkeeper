# shellcheck shell=sh
# Runs inside the OpenHarmony session with OH's own sh (ohos/run.py --probe),
# once samgr is ready: OH's core system abilities register with samgr and
# hidumper (itself a system ability samgr starts on demand) lists them.
want="SystemAbilityManager AccessTokenManagerService BundleMgr AbilityManagerService RenderService"
i=0
while [ $i -lt 150 ]; do
  list=$(/system/bin/hidumper -ls 2>&1)
  missing=""
  for sa in $want; do
    echo "$list" | grep -qw "$sa" || missing="$missing $sa"
  done
  if [ -z "$missing" ]; then
    echo "$list"
    exit 0
  fi
  i=$((i + 1))
  sleep 0.2
done
echo "not registered:$missing"
echo "$list"
exit 1
