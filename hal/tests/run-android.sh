#!/usr/bin/env bash
# Run our graphics HALs and an on-device test without booting Android.
#
#   hal/tests/run-android.sh [-w] [-e VAR=val]... -- /vendor/bin/hwc_test [args]
#
# -w binds the host compositor's Wayland socket (that one file) at
# /dev/socket/wayland-host, for the composer's Wayland backend.
#
# Builds a throwaway root in a rootless user+mount+pid namespace (inside its
# own transient systemd user unit): the image's /system, /system_ext,
# /product and flattened /apex, /vendor = out/vendor-overlay plus the test
# binaries in /vendor/bin, a private binderfs, and a generated /linkerconfig.
# It then starts the image's servicemanager, our allocator and composer, and
# runs the command. There is no init, logd or property service:
# tests/android/haltest_shim.c (LD_PRELOAD) mirrors logs to stderr and fakes
# properties (LINUXAPK_PROPS="name=value;...", see -e). Frame dumps land in out/hal-test/frames.
#
# Host safety (docs/NOTES.md): no /sys at all, /dev is a tmpfs with a few
# individually bound nodes, nothing from $HOME or /run/user is mounted
# (except the single Wayland socket file with -w).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
main=$(cd "$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
out=${OUT:-$main/out}

case ${1:-} in
  --inner) shift ;;
  --ns)
    shift
    exec unshare --user --map-root-user --mount --pid --fork --kill-child -- "$0" --inner "$@"
    ;;
  *)
    [[ -d $out/root/apex/com.android.runtime ]] || {
      echo "missing $out/root/apex (run tools/flatten-apex.sh)" >&2
      exit 1
    }
    mkdir -p "$out/hal-test/frames"
    # The host compositor's socket (only this one file is bound in, with -w).
    wl=""
    [[ -n ${WAYLAND_DISPLAY:-} ]] && wl=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/$WAYLAND_DISPLAY
    [[ ${WAYLAND_DISPLAY:-} == /* ]] && wl=$WAYLAND_DISPLAY
    exec systemd-run --user --quiet --collect --wait --pipe --same-dir \
      --unit="linuxapk-haltest-$$" -p KillMode=control-group -p RuntimeMaxSec=150 \
      -E OUT="$out" -E HOST_WAYLAND_SOCKET="$wl" -- "$0" --ns "$@"
    ;;
esac

envs=()
wayland=0
while [[ $# -gt 0 ]]; do
  case $1 in
    -e)
      envs+=("$2")
      shift 2
      ;;
    -w)
      wayland=1
      shift
      ;;
    --)
      shift
      break
      ;;
    *) break ;;
  esac
done

mount --make-rprivate /
R=$(mktemp -d /tmp/haltest-root.XXXXXX)
mount -t tmpfs -o mode=0755 root "$R"
mkdir -p "$R"/{system,system_ext,product,vendor,apex,linkerconfig,dev,proc,tmp,data} \
  "$R"/data/local/tmp "$R"/data/vendor/linuxapk/frames "$R/.vendor-test/bin"

ro_bind() {
  mount --bind "$1" "$2"
  mount -o remount,bind,ro "$2"
}
ro_bind "$out/img/system/system" "$R/system"
ro_bind "$out/img/system_ext" "$R/system_ext"
ro_bind "$out/img/product" "$R/product"
ro_bind "$out/root/apex" "$R/apex"
# /vendor: our overlay, plus test binaries in /vendor/bin.
cp "$out/hal-build/tests/"*_test "$R/.vendor-test/bin/"
# /data is a permitted linker path for system binaries.
cp "$out/hal-build/tests/libhaltest_shim.so" "$R/data/local/tmp/"
# Vendor processes may preload it from /vendor/lib64 (it mirrors liblog to stderr).
mkdir -p "$R/.vendor-test/lib64" "$R/.vendor-test/etc/vintf"
# Base device manifest (libvintf ignores fragments without one). In a real
# session the image's /vendor/etc/vintf/manifest.xml plays this role.
[[ -e $out/vendor-overlay/etc/vintf/manifest.xml ]] ||
  echo '<manifest version="1.0" type="device" target-level="202604"/>' \
    >"$R/.vendor-test/etc/vintf/manifest.xml"
cp "$out/hal-build/tests/libhaltest_shim.so" "$R/.vendor-test/lib64/"
mount -t overlay overlay -o "ro,lowerdir=$R/.vendor-test:$out/vendor-overlay" "$R/vendor"
mount --bind "$out/hal-test/frames" "$R/data/vendor/linuxapk/frames"
mount -t tmpfs tmp "$R/tmp"
mount -t tmpfs linkerconfig "$R/linkerconfig"

mount -t tmpfs -o mode=0755 dev "$R/dev"
for n in null zero full random urandom udmabuf; do
  [[ -e /dev/$n ]] || continue
  touch "$R/dev/$n"
  mount --bind "/dev/$n" "$R/dev/$n"
done
mkdir -p "$R/dev/binderfs"
mount -t binder binder "$R/dev/binderfs"
for b in binder hwbinder vndbinder; do ln -s "binderfs/$b" "$R/dev/$b"; done
mkdir -p "$R/dev/socket"
if ((wayland)); then
  [[ -S ${HOST_WAYLAND_SOCKET:-} ]] || {
    echo "-w: no host Wayland socket" >&2
    exit 1
  }
  touch "$R/dev/socket/wayland-host"
  mount --bind "$HOST_WAYLAND_SOCKET" "$R/dev/socket/wayland-host"
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
export PATH=/system/bin:/vendor/bin ANDROID_ROOT=/system ANDROID_DATA=/data \
  ANDROID_ART_ROOT=/apex/com.android.art ANDROID_I18N_ROOT=/apex/com.android.i18n \
  ANDROID_TZDATA_ROOT=/apex/com.android.tzdata
for e in "${envs[@]}"; do export "${e?}"; done

/apex/com.android.runtime/bin/linkerconfig --target /linkerconfig >/tmp/linkerconfig.log 2>&1 ||
  {
    echo "linkerconfig failed:"
    cat /tmp/linkerconfig.log
  } >&2

LD_PRELOAD=/data/local/tmp/libhaltest_shim.so /system/bin/servicemanager &
for _ in $(seq 50); do
  [[ -e /dev/binderfs/binder ]] && break
  sleep 0.1
done
sleep 0.3
export VENDOR_PRELOAD=/vendor/lib64/libhaltest_shim.so
LD_PRELOAD=$VENDOR_PRELOAD /vendor/bin/hw/android.hardware.graphics.allocator-service.linuxapk &
LD_PRELOAD=$VENDOR_PRELOAD /vendor/bin/hw/android.hardware.graphics.composer3-service.linuxapk &
sleep 0.5
timeout 90 env LD_PRELOAD=$VENDOR_PRELOAD "$@"
