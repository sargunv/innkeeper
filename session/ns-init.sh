#!/usr/bin/env bash
# Runs as pid 1 inside the session's user, mount, pid and cgroup namespaces
# (started by linuxapk-session). Assembles the Android root on a tmpfs,
# pivots into it and execs Android init (second stage).
#
# Inputs (environment): LINUXAPK_REPO LINUXAPK_OUT LINUXAPK_RUN LINUXAPK_DATA
set -euo pipefail

repo=$LINUXAPK_REPO out=$LINUXAPK_OUT run=$LINUXAPK_RUN data=$LINUXAPK_DATA
img=$out/img
apex=$out/root/apex
ovl=$repo/session/rootfs-overlay

log() { echo "ns-init: $*" >&2; }

# Safety: never run Android init outside a fresh pid namespace.
if [[ $$ != 1 ]]; then
  log "refusing to continue: not pid 1 in a new pid namespace (pid $$)"; exit 1
fi
[[ $(cat /proc/self/uid_map | awk '{print $2; exit}') != 0 ]] || {
  log "refusing to continue: in the initial user namespace"; exit 1; }

mount --make-rprivate /

R=$run/root
mkdir -p "$R"
mount -t tmpfs -o mode=0755 rootfs "$R"

# Mount an overlayfs of the existing dirs among "$@" (topmost first) on $dst,
# or a plain read-only bind if only one layer exists.
layer_mount() {
  local dst=$1; shift
  local layers=()
  for l in "$@"; do [[ -d $l ]] && layers+=("$l"); done
  mkdir -p "$dst"
  if (( ${#layers[@]} == 1 )); then
    mount --bind "${layers[0]}" "$dst"
    mount -o remount,bind,ro "$dst"
  else
    local IFS=:
    mount -t overlay overlay -o "ro,lowerdir=${layers[*]}" "$dst"
  fi
}

# --- partitions -------------------------------------------------------------
layer_mount "$R/system" "$ovl/system" "$out/session/overlay/system" "$img/system/system"
layer_mount "$R/vendor" "$ovl/vendor" "$out/vendor-overlay" "$out/session/overlay/vendor" "$img/vendor"
layer_mount "$R/system_ext" "$ovl/system_ext" "$img/system_ext"
layer_mount "$R/product" "$ovl/product" "$img/product"
layer_mount "$R/odm" "$ovl/odm" "$img/vendor/odm"
[[ -d $img/system_dlkm ]] && layer_mount "$R/system_dlkm" "$img/system_dlkm"

# Top-level entries of the system-as-root image (symlinks, init.environ.rc,
# empty mount points). Partitions were handled above.
for e in "$img/system"/* "$img/system"/.[!.]*; do
  [[ -e $e || -L $e ]] || continue
  n=$(basename "$e")
  [[ -e $R/$n ]] && continue
  if [[ -L $e ]]; then cp -P "$e" "$R/$n"
  elif [[ -d $e ]]; then mkdir -m "$(stat -c %a "$e")" "$R/$n"
  else cp -p "$e" "$R/$n"
  fi
done
rm -rf "$R/lost+found"
# Root-level overrides (e.g. init.environ.rc additions).
if [[ -d $ovl/root ]]; then cp -a "$ovl/root/." "$R/"; fi

# --- /apex: pre-flattened APEXes instead of apexd ---------------------------
mount -t tmpfs -o mode=0755 apex "$R/apex"
for d in "$apex"/*/; do
  n=$(basename "$d")
  mkdir "$R/apex/$n"
  mount --bind "$d" "$R/apex/$n"
  mount -o remount,bind,ro "$R/apex/$n"
done
cp "$apex/apex-info-list.xml" "$R/apex/"
chmod 0644 "$R/apex/apex-info-list.xml"
# init bind-mounts /bootstrap-apex over /apex in the bootstrap mount namespace;
# give it the same full set so both namespaces see every APEX.
mkdir -p "$R/bootstrap-apex"
mount --rbind "$R/apex" "$R/bootstrap-apex"

# --- writable state -----------------------------------------------------------
mkdir -p "$data"
mount --bind "$data" "$R/data"
for d in linkerconfig metadata cache mnt storage tmp data_mirror; do
  mkdir -p "$R/$d"
  mount -t tmpfs -o mode=0755 "$d" "$R/$d"
done
chmod 0770 "$R/cache"

# --- /dev -------------------------------------------------------------------
mount -t tmpfs -o mode=0755,nosuid dev "$R/dev"
for n in null zero full random urandom tty fuse; do
  [[ -e /dev/$n ]] || continue
  touch "$R/dev/$n"
  mount --bind "/dev/$n" "$R/dev/$n"
done
# Android init logs to /dev/kmsg: hand it the host-side FIFO instead.
touch "$R/dev/kmsg"
mount --bind "$run/kmsg" "$R/dev/kmsg"
mkdir -p "$R/dev/pts" "$R/dev/socket"
mount -t devpts -o newinstance,ptmxmode=0666,mode=0620,gid=5 devpts "$R/dev/pts"
ln -s pts/ptmx "$R/dev/ptmx"
if [[ -d /dev/dri ]]; then
  mkdir -p "$R/dev/dri"
  mount --rbind /dev/dri "$R/dev/dri"
fi
mkdir -p "$R/dev/binderfs"   # init mounts binderfs itself (init.rc "on init")

# --- /proc, /sys -------------------------------------------------------------
mount -t proc -o nosuid,nodev,noexec proc "$R/proc"
mount --bind "$run/cmdline" "$R/proc/cmdline"
[[ -e $R/proc/bootconfig ]] && mount --bind "$run/bootconfig" "$R/proc/bootconfig"

mount --rbind /sys "$R/sys"
mount -o remount,bind,ro "$R/sys" || true
# Hide SELinux: libselinux then reports it as disabled (see shim/selinux_shim.c).
mount -t tmpfs -o ro,mode=0755 noselinux "$R/sys/fs/selinux"
# A cgroup2 view rooted at this session's cgroup (we are in a new cgroup ns).
mount -t cgroup2 -o nosuid,nodev,noexec cgroup2 "$R/sys/fs/cgroup" || log "cgroup2 mount failed"

# --- go -----------------------------------------------------------------------
cd "$R"
pivot_root . .
umount -l .
cd /

for v in $(compgen -e); do unset "$v"; done
export PATH=/system/bin:/system/xbin:/vendor/bin
export LD_PRELOAD=liblinuxapk_shim.so
exec /system/bin/init second_stage
