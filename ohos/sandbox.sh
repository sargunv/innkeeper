#!/usr/bin/env bash
# Inside the Flatpak sandbox: make its `/` OpenHarmony's root, then run OH's
# init under the supervisor (pivot-exec, then the broker). Started by
# ohos/run.py; never run it on the host.
#
# usage: sandbox.sh <image-dir> <layer-dir> <data-dir> <pivot-exec>
#
# <image-dir> holds the pinned image's partitions (image/ohos/fetch.sh):
# system/ is system.img's root (system-as-root), vendor/ is vendor.img.
# <layer-dir> is ohos:build's output: bin/linuxapk-broker and sys_prod/, our
# files on OH's own product partition (empty in the image, but on init's cfg
# path and the linker's search path), so the image is never changed.
# The sandbox's / is a writable tmpfs, so OH's root is built from symlinks
# beside the runtime's /usr, as the Android session does
# (host/launcher/src/flatpak.rs).
set -euo pipefail
usage="usage: sandbox.sh <image-dir> <layer-dir> <data-dir> <pivot-exec>"
img=${1:?$usage} layer=${2:?$usage} data=${3:?$usage} pivot=${4:?$usage}
[[ -f /.flatpak-info ]] || {
  echo 'sandbox.sh: not in a Flatpak sandbox' >&2
  exit 1
}

ln -sfn "$img/system/system" /system
ln -sfn "$img/vendor" /vendor
ln -sfn "$layer/sys_prod" /sys_prod
mkdir -p "$data"
ln -sfn "$data" /data
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
} <>/dev/kmsg >>"${LINUXAPK_OHOS_KMSG:-/dev/null}" &
# The binder nodes the broker serves.
for n in binder hwbinder vndbinder; do
  : >"/dev/$n"
  chmod 666 "/dev/$n"
done

exec env -i \
  PATH=/system/bin:/vendor/bin \
  LINUXAPK_GUEST=ohos \
  LINUXAPK_BROKER="$layer/bin/linuxapk-broker" \
  LD_PRELOAD=/sys_prod/lib64/liblinuxapk_ohos_shim.so \
  LINUXAPK_ROOT_MAP="$img/system/system=/system
$img/vendor=/vendor
$layer/sys_prod=/sys_prod
$data=/data" \
  ${LINUXAPK_BROKER_DEBUG:+LINUXAPK_BROKER_DEBUG="$LINUXAPK_BROKER_DEBUG"} \
  "$pivot" / /system/bin/init --second-stage 0
