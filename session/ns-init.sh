#!/usr/bin/env bash
# Runs as pid 1 inside the session's user, mount, pid and cgroup namespaces
# (started by linuxapk-session). Assembles the Android root on a tmpfs, then
# pivot-exec pivots into it and starts Android init (second stage) in nested,
# locked namespaces (see helper/pivot-exec.c).
#
# Inputs (environment): LINUXAPK_REPO LINUXAPK_OUT LINUXAPK_RUN LINUXAPK_DATA
#                       LINUXAPK_HOLD=1 to stop before exec'ing init
set -euo pipefail

repo=$LINUXAPK_REPO out=$LINUXAPK_OUT run=$LINUXAPK_RUN data=$LINUXAPK_DATA
img=$out/img
apex=$out/root/apex
ovl=$repo/session/rootfs-overlay

log() { echo "ns-init: $*" >&2; }

# Safety: never run Android init outside a fresh pid namespace.
if [[ $$ != 1 ]]; then
  log "refusing to continue: not pid 1 in a new pid namespace (pid $$)"
  exit 1
fi
[[ $(cat /proc/self/uid_map | awk '{print $2; exit}') != 0 ]] || {
  log "refusing to continue: in the initial user namespace"
  exit 1
}

mount --make-rprivate /

R=$run/root
mkdir -p "$R"
mount -t tmpfs -o mode=0755 rootfs "$R"

# Mount an overlayfs of the existing dirs among "$@" (topmost first) on $dst,
# or a plain read-only bind if only one layer exists.
layer_mount() {
  local dst=$1
  shift
  local layers=()
  for l in "$@"; do [[ -d $l ]] && layers+=("$l"); done
  mkdir -p "$dst"
  if ((${#layers[@]} == 1)); then
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
layer_mount "$R/system_ext" "$ovl/system_ext" "$out/session/overlay/system_ext" "$img/system_ext"
layer_mount "$R/product" "$ovl/product" "$out/session/overlay/product" "$img/product"
layer_mount "$R/odm" "$ovl/odm" "$out/session/overlay/odm" "$img/vendor/odm"
[[ -d $img/system_dlkm ]] && layer_mount "$R/system_dlkm" "$img/system_dlkm"

# Top-level entries of the system-as-root image (symlinks, init.environ.rc,
# empty mount points). Partitions were handled above.
for e in "$img/system"/* "$img/system"/.[!.]*; do
  [[ -e $e || -L $e ]] || continue
  n=$(basename "$e")
  [[ -e $R/$n ]] && continue
  if [[ -L $e ]]; then
    cp -P "$e" "$R/$n"
  elif [[ -d $e ]]; then
    mkdir -m "$(stat -c %a "$e")" "$R/$n"
  else
    cp -p "$e" "$R/$n"
  fi
done
rm -rf "$R/lost+found"
# Root-level overrides (e.g. init.environ.rc additions).
if [[ -d $ovl/root ]]; then cp -a "$ovl/root/." "$R/"; fi

# --- /apex: pre-flattened APEXes instead of apexd ---------------------------
# Init mounts an empty tmpfs over /apex early in second stage, so the tree is
# staged at /linuxapk/apex and init.rc bind-mounts it over /apex in
# early-init. It is one mount of the whole tree (no submounts), so
# non-recursive views of it stay complete.
# Overrides for APEX contents (e.g. rc files) layer on top like partitions.
mkdir -p "$R/apex" "$R/bootstrap-apex"
layer_mount "$R/linuxapk/apex" "$ovl/apex" "$apex"
# For /system/bin/linuxapk-apexd: what apexd would see in /proc/mounts had it
# mounted each APEX itself (erofs file-backed: "<file> /apex/<name>@<ver>").
sed -n 's/.*moduleName="\([^"]*\)" modulePath="\([^"]*\)".* versionCode="\([0-9]*\)".*/\2 \/apex\/\1@\3 erofs ro,fsoffset=0 0 0/p' \
  "$apex/apex-info-list.xml" >"$R/linuxapk/apexd-mounts"

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
# udmabuf: dma-bufs from memfds for the gralloc HAL (logind ACL on the host).
for n in null zero full random urandom fuse udmabuf; do
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
mkdir -p "$R/dev/binderfs" # init mounts binderfs itself (init.rc "on init")

# --- /proc, /sys -------------------------------------------------------------
# A clean procfs for this (outer) pid namespace. pivot-exec mounts the nested
# namespace's own /proc on top (the kernel only allows that while a fully
# visible procfs exists, so nothing may be mounted over this one) and binds
# these stand-ins over it:
#  - cmdline/bootconfig carry our androidboot.* values;
#  - init insists on raising ASLR entropy (fatal if it can't), so it gets a
#    writable mmap_rnd_bits; the host's real setting applies regardless.
mount -t proc -o nosuid,nodev,noexec proc "$R/proc"
mkdir -p "$R/linuxapk/proc/sys/vm"
cp "$run/cmdline" "$run/bootconfig" "$R/linuxapk/proc/"
echo 32 >"$R/linuxapk/proc/sys/vm/mmap_rnd_bits"

# Host safety: inside this user namespace we are root over everything the host
# user owns. sysfs submounts expose such files: fusectl's per-connection
# "abort" files belong to the FUSE mount's owner, and vold's
# AbortFuseConnections() on shutdown wrote to them, killing the host's AppImage
# and xdg-document-portal FUSE mounts. A plain bind isn't allowed here (the
# submounts are locked to /sys), so rbind it, then cover every submount with an
# empty read-only tmpfs and make the whole tree read-only.
mount --rbind /sys "$R/sys"
mapfile -t sys_submounts < <(findmnt -rn -o TARGET -R "$R/sys" | tail -n +2 | sort -r)
for m in "${sys_submounts[@]}"; do
  mount -t tmpfs -o ro,nosuid,nodev,noexec,mode=0755 masked "$m"
done
mount -o remount,bind,ro,nosuid,nodev,noexec "$R/sys"
# Hiding /sys/fs/selinux (now an empty tmpfs) makes libselinux report SELinux
# as disabled (see shim/). /sys/fs/cgroup gets a writable tmpfs: init
# (libprocessgroup) mounts cgroup2 there, rooted at this session's own
# delegated cgroup because we're in a new cgroup namespace.
mount -t tmpfs -o mode=0755,nosuid,nodev,noexec nocgroup "$R/sys/fs/cgroup"
# /sys/fs/bpf: a writable session-private tmpfs. Pinned objects of the bpf()
# emulator (helper/bpfemu.c) are plain files here.
mount -t tmpfs -o mode=1777,nosuid,nodev,noexec bpf "$R/sys/fs/bpf"

findmnt -R -o TARGET,FSTYPE,OPTIONS "$R/sys" >"$run/sys-mounts.txt" || true

# Refuse to boot if any host-user-owned control file is still reachable.
if [[ -n $(ls -A "$R/sys/fs/fuse/connections" 2>/dev/null) ]]; then
  log "refusing to continue: host FUSE connections visible in the session"
  exit 1
fi
if touch "$R/sys/.linuxapk-rw-probe" 2>/dev/null; then
  log "refusing to continue: /sys is writable in the session"
  exit 1
fi

# --- go -----------------------------------------------------------------------
if [[ ${LINUXAPK_HOLD:-} == 1 ]]; then
  # Debug: assemble the root but don't run init; "linuxapk-session exec" works.
  log "holding before init (LINUXAPK_HOLD=1)"
  # init normally binds the staged APEX tree over /apex; do it here so
  # /system/bin/linker64 (-> /apex/com.android.runtime/...) resolves.
  mkdir -p "$R/apex" && mount --bind "$R/linuxapk/apex" "$R/apex"
  exec "$out/session/pivot-exec" "$R" /system/bin/sh -c "while :; do /system/bin/sleep 3600; done"
fi

for v in $(compgen -e); do unset "$v"; done
export PATH=/system/bin:/system/xbin:/vendor/bin
# Absolute path: a bare name is not found from APEX linker namespaces.
export LD_PRELOAD=/system/lib64/liblinuxapk_shim.so
exec "$out/session/pivot-exec" "$R" /system/bin/init second_stage
