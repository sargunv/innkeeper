#!/usr/bin/env bash
# Inside the Flatpak sandbox: make its `/` OpenHarmony's root, then run OH's
# init under the broker. Started by ohos/run.py; never run it on the host.
#
# usage: sandbox.sh <image-dir> <data-dir>
#
# <image-dir> is image/ohos/fetch.sh's img/. Our layer (build:ohos) and the
# broker come from the stage, at /app.
# With INNKEEPER_OHOS_BRIDGE=<log> set, the bridge runs beside OH and the
# display shows in a desktop window.
# The sandbox's / is a writable tmpfs, so OH's root is built from symlinks
# beside the runtime's /usr, as the Android session does
# (host/launcher/src/sandbox.rs). The image is never changed.
set -euo pipefail
usage="usage: sandbox.sh <image-dir> <data-dir>"
img=${1:?$usage} data=${2:?$usage}
layer=/app/share/innkeeper/ohos
[[ -f /.flatpak-info ]] || {
  echo 'sandbox.sh: not in a Flatpak sandbox' >&2
  exit 1
}

ln -sfn "$img/system/system" /system
# overlay <dir> <image-dir> <layer-dir>: <dir> gets the image's entries as
# symlinks, with the layer's files in their place (and its directories
# overlaid the same way).
overlay() {
  local dir=$1 from=$2 over=$3 e n
  mkdir -p "$dir"
  for e in "$from"/*; do
    n=${e##*/}
    [[ -e $over/$n ]] || ln -s "$e" "$dir/$n"
  done
  for e in "$over"/*; do
    n=${e##*/}
    if [[ -d $e && -d $from/$n ]]; then
      overlay "$dir/$n" "$from/$n" "$e"
    else
      ln -s "$e" "$dir/$n"
    fi
  done
}
overlay /vendor "$img/vendor" "$layer/vendor"
# The composer's panel (ohos/vendor/display) and DMS's density, which DMS
# takes from its config's <dpi> alone. At 320 dpi a 720x1280 panel is
# 360x640 vp: a phone.
display=720x1280@320
dms=etc/window/resources
for d in etc etc/window $dms; do
  [[ -L /vendor/$d ]] || continue
  rm /vendor/"$d"
  mkdir /vendor/"$d"
  for e in "$img/vendor/$d"/*; do ln -s "$e" "/vendor/$d/${e##*/}"; done
done
rm -f /vendor/$dms/display_manager_config.xml
sed "s|<dpi>[0-9]*</dpi>|<dpi>${display#*@}</dpi>|" \
  "$img/vendor/$dms/display_manager_config.xml" >/vendor/$dms/display_manager_config.xml
ln -sfn "$layer/sys_prod" /sys_prod
mkdir -p "$data"
ln -sfn "$data" /data
# Each app's own /data/storage, which the shim keeps open in app processes.
ln -sfn /proc/self/fd/990 "$data/storage"
# The image's other top-level entries where the sandbox has none: /chipset
# and the empty mount points. The sandbox keeps its /bin, /etc, /lib64 and
# /usr for the glibc side.
for e in "$img"/system/*; do
  n=${e##*/}
  [[ -e /$n || -L /$n ]] && continue
  if [[ -L $e ]]; then
    ln -s "$(readlink "$e")" "/$n"
  elif [[ -d $e ]]; then
    mkdir -p "/$n"
  fi
done
# OH binaries ask for /lib/ld-musl-x86_64.so.1, and the image's /lib is
# /system/lib. The sandbox's /lib is a symlink to /usr/lib: make it a
# directory of the same entries plus the musl loader.
if [[ -L /lib ]]; then
  rm /lib
  mkdir /lib
  for e in /usr/lib/*; do ln -s "$e" "/lib/${e##*/}"; done
fi
ln -sfn /system/lib/ld-musl-x86_64.so.1 /lib/ld-musl-x86_64.so.1
# /etc is /system/etc in the image. The sandbox's /etc gains the entries it
# lacks (its own binds, such as resolv.conf, stay); the broker serves
# /etc/passwd and /etc/group from /system/etc.
for e in /system/etc/*; do
  n=${e##*/}
  [[ -e /etc/$n || -L /etc/$n ]] || ln -s "$e" "/etc/$n"
done
# Init and the services log to /dev/kmsg: a FIFO into the run's log, opened
# read-write so it never sees end of file, and bounded (a crash loop logs
# fast); past the bound it is drained so writers never block.
rm -f /dev/kmsg
mkfifo -m 666 /dev/kmsg
{
  head -c 64M
  cat >/dev/null
} <>/dev/kmsg >>"${INNKEEPER_OHOS_KMSG:-/dev/null}" &
# The binder nodes and the access-token node, which the broker serves.
for n in binder hwbinder vndbinder access_token_id; do
  : >"/dev/$n"
  chmod 666 "/dev/$n"
done
# DRM: only the first render node opens (the broker's policy; empty if there
# is none), never a card node, whose KMS is the host's display.
render=$(compgen -G '/dev/dri/renderD*' | head -n 1 || true)
if [[ -n ${INNKEEPER_OHOS_BRIDGE:-} ]]; then
  # A test session of profile `ohos`, whose run dir the bridge finds as the
  # launcher's would be (host/launcher/src/paths.rs).
  run=$XDG_RUNTIME_DIR/app/$FLATPAK_ID/innkeeper-ohos
  mkdir -p "$run/bridge" "${XDG_STATE_HOME:-$HOME/.local/state}/innkeeper-ohos"
  INNKEEPER_PROFILE=ohos INNKEEPER_TEST=1 /app/libexec/innkeeper/innkeeper-bridge \
    >>"$INNKEEPER_OHOS_BRIDGE" 2>&1 &
  mkdir -p /dev/socket
  ln -sfn "$run/bridge" /dev/socket/innkeeper
fi

cd /
exec env -i \
  PATH=/system/bin:/vendor/bin \
  INNKEEPER_GUEST=ohos \
  LIBGL_ALWAYS_SOFTWARE=1 \
  INNKEEPER_OHOS_DISPLAY="$display" \
  INNKEEPER_RENDER_NODE="$render" \
  INNKEEPER_ROOT_MAP="$img/system/system=/system
$img/vendor=/vendor
$layer/vendor=/vendor
$layer/sys_prod=/sys_prod
$data=/data" \
  ${INNKEEPER_BROKER_DEBUG:+INNKEEPER_BROKER_DEBUG="$INNKEEPER_BROKER_DEBUG"} \
  /app/libexec/innkeeper/innkeeper-brk --preload /sys_prod/lib64/libinnkeeper_ohos_shim.so \
  /system/bin/init --second-stage 0
