# shellcheck shell=bash
# Shared by the bionic test harnesses (tests/bionic/run.sh,
# supervisor/tests/binder/run.sh): run Android binaries from the session's
# image in a throwaway root, without booting Android.
#
# Sourcing it sets:
#   repo        this checkout
#   stage       out/stage: pivot-exec, the broker and our /vendor come from there
#   scratch     out/test/bionic: the harnesses' roots and run.sh's output
#   pivot_exec, broker, stage_vendor, props_shim (tests/bionic/props-shim.c,
#               from the NDK superbuild)

# shellcheck disable=SC2034 # for the scripts that source this
{
  repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
  stage=$repo/out/stage
  scratch=$repo/out/test/bionic
  pivot_exec=$stage/libexec/linuxapk/pivot-exec
  broker=$stage/share/linuxapk/android/system/bin/linuxapk-broker
  stage_vendor=$stage/share/linuxapk/android/vendor
  props_shim=$repo/out/build/android/tests/bionic/libprops_shim.so
}

# Check that the build is staged and the image prepared, and set img and apex
# to the image's partitions and flattened APEXes (the paths
# `linuxapk-session image` prints).
img="" apex=""
bionic_prepare() {
  local f k v
  for f in "$pivot_exec" "$broker" "$props_shim"; do
    [[ -e $f ]] || {
      echo "no $f (run \`mise run build\`)" >&2
      return 1
    }
  done
  while IFS='=' read -r k v; do
    case $k in
      img) img=$v ;;
      apex) apex=$v ;;
    esac
  done < <("$stage/libexec/linuxapk/linuxapk-session" image)
  [[ -d $img/system/system && -f $apex/apex-info-list.xml ]] || {
    echo "image not prepared at $img (run \`mise run session -- prepare\`)" >&2
    return 1
  }
}

# bionic_unit NAME MAX_SECONDS [-E VAR=val]... -- ARG...: rerun the calling
# script as `SCRIPT --inner ARG...`, as root of a private user, mount, pid
# and cgroup namespace, in a transient systemd user unit with a memory cap and
# a run time limit. The unit's cgroup is delegated, so a harness may mount it
# (cgroup2) as the root's /sys/fs/cgroup. The unit gets BIONIC_IMG and
# BIONIC_APEX (from bionic_prepare) and each -E.
bionic_unit() {
  local name=$1 max=$2 envs=()
  shift 2
  while [[ $1 == -E ]]; do
    envs+=(-E "$2")
    shift 2
  done
  shift
  systemd-run --user --quiet --collect --wait --pipe --same-dir \
    --unit="$name-$$" -p KillMode=control-group -p RuntimeMaxSec="$max" \
    -p MemoryHigh=3G -p MemoryMax=4G -p Delegate=yes \
    -E BIONIC_IMG="$img" -E BIONIC_APEX="$apex" "${envs[@]}" \
    -- unshare --user --map-root-user --mount --pid --cgroup --fork --kill-child -- "$0" --inner "$@"
}

ro_bind() {
  mount --bind "$1" "$2"
  mount -o remount,bind,ro "$2"
}

# bionic_root ROOT VENDOR_DIR...: assemble an Android root at ROOT (run as
# root of a private user+mount namespace). /vendor is the VENDOR_DIRs stacked
# read-only, first on top. The props shim is in /system/lib64 and
# /vendor/lib64, so LD_PRELOAD=libprops_shim.so finds it in either linker
# namespace.
#
# Host safety (AGENTS.md): /dev is a tmpfs with a few individually bound
# nodes, /sys is an empty tmpfs (plus /sys/fs/bpf for pivot-exec's bpf
# emulator), and nothing from $HOME or /run/user is mounted.
bionic_root() {
  local R=$1
  shift
  mount --make-rprivate /
  mkdir -p "$R"
  mount -t tmpfs -o mode=0755 root "$R"
  mkdir -p "$R"/{system,system_ext,product,vendor,apex,linkerconfig,dev,proc,sys,tmp,data/local/tmp}
  ro_bind "$img/system_ext" "$R/system_ext"
  ro_bind "$img/product" "$R/product"
  ro_bind "$apex" "$R/apex"
  mkdir -p "$R/.shim/lib64"
  cp "$props_shim" "$R/.shim/lib64/"
  # lowerdir+= per layer: fsconfig caps a single option value at 256 bytes.
  mount -t overlay overlay -o "ro,lowerdir+=$R/.shim,lowerdir+=$img/system/system" "$R/system"
  mount -t overlay overlay -o "ro,lowerdir+=$R/.shim$(printf ',lowerdir+=%s' "$@")" "$R/vendor"
  mount -t tmpfs -o mode=0755 dev "$R/dev"
  mkdir -p "$R/dev/socket"
  local n
  for n in null zero full random urandom; do
    touch "$R/dev/$n"
    mount --bind "/dev/$n" "$R/dev/$n"
  done
  mount -t proc proc "$R/proc"
  mount -t tmpfs -o mode=0755 sys "$R/sys"
  mkdir -p "$R/sys/fs/bpf"
  mount -t tmpfs -o mode=1777 bpf "$R/sys/fs/bpf"
  # Top-level symlinks of the system-as-root image (/bin, /etc, ...).
  local e
  for e in "$img/system"/*; do
    n=$(basename "$e")
    [[ -L $e && ! -e $R/$n ]] && cp -P "$e" "$R/$n"
  done
  return 0
}

# Readable stand-ins for libbinder's access("/dev/binder") check (the seccomp
# filter routes every open of them to the broker), and the driver features
# binderfs would report, as host/launcher/ns-init.sh makes them.
bionic_broker_nodes() {
  local d
  for d in binder hwbinder vndbinder; do
    : >"$1/dev/$d"
    chmod 0666 "$1/dev/$d"
  done
  mkdir -p "$1/dev/binderfs/features"
  for d in oneway_spam_detection extended_error freeze_notification; do
    echo 1 >"$1/dev/binderfs/features/$d"
  done
}

# Clear the environment and set what Android processes expect.
bionic_env() {
  local v
  for v in $(compgen -e); do unset "$v"; done
  export PATH=/system/bin:/vendor/bin ANDROID_ROOT=/system ANDROID_DATA=/data \
    ANDROID_ART_ROOT=/apex/com.android.art ANDROID_I18N_ROOT=/apex/com.android.i18n \
    ANDROID_TZDATA_ROOT=/apex/com.android.tzdata TMPDIR=/data/local/tmp
}
