#!/usr/bin/env bash
# Runs as pid 1 inside the session's user, mount, pid and cgroup namespaces
# (started by linuxapk-session). Assembles the Android root on a tmpfs, then
# pivot-exec pivots into it and starts Android init (second stage) in nested,
# locked namespaces (see supervisor/pivot-exec/pivot-exec.c).
#
#   ns-init.sh <session.conf>
#
# session.conf (written by linuxapk-session start) sets: run, data; img,
# apexdir, derived (the image's partitions, flattened APEXes and derived
# layers); android (our Android tree); addons (addon layers, one dir each);
# uid_mode; broker_debug; sandbox (flatpak or empty) and root (Flatpak mode's
# composed root).
# shellcheck disable=SC2154 # session.conf's variables
set -euo pipefail

# util-linux 2.41.5's fd-based mount API (fsopen/fsconfig/fsmount) fails
# EINVAL for an overlay mount inside a user namespace on this host, which
# breaks the partition overlays below; the classic mount(2) syscall works, so
# force libmount to use it. Harmless for the other (tmpfs/bind/proc) mounts.
export LIBMOUNT_FORCE_MOUNT2=always

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=/dev/null
source "${1:?usage: ns-init.sh <session.conf>}"
# The derived layer for this uid mode (image/derive.sh).
uid_layer=$derived/$uid_mode
addon_layers=()
for a in "$addons"/*/; do [[ -d $a ]] && addon_layers+=("${a%/}"); done

log() { echo "ns-init: $*" >&2; }

# Starts Android init (second stage) through pivot-exec in root $1, with a
# clean environment plus what pivot-exec, the broker and the shim read.
start_init() {
  local v root=$1
  shift
  for v in $(compgen -e); do unset "$v"; done
  export PATH=/system/bin:/system/xbin:/vendor/bin
  # Absolute path: a bare name is not found from APEX linker namespaces.
  export LD_PRELOAD=/system/lib64/liblinuxapk_shim.so
  export LINUXAPK_BROKER=/system/bin/linuxapk-broker
  [[ -n $broker_debug ]] && export LINUXAPK_BROKER_DEBUG=$broker_debug
  [[ $uid_mode == single ]] && export LINUXAPK_SINGLE_UID=1
  for v in "$@"; do export "${v?}"; done
  exec "$here/pivot-exec" "$root" /system/bin/init second_stage
}

# Binder is served in userspace (supervisor/README.md), so /dev ($1) gets
# dummy, readable binder device nodes, made before the seccomp filter is
# installed: libbinder gates on access("/dev/binder", R_OK) in
# selfIfKernelBinderEnabled(), the supervisor intercepts real opens and
# injects a memfd, and init cannot create them itself (its own open of
# /dev/binder is intercepted). binderfs/features says what binderfs would
# report about the driver (libbinder reads these): the broker implements each.
binder_nodes() {
  local d
  for d in binder hwbinder vndbinder; do
    : >"$1/$d"
    chmod 0666 "$1/$d"
  done
  mkdir -p "$1/binderfs/features"
  for d in oneway_spam_detection extended_error freeze_notification; do
    echo 1 >"$1/binderfs/features/$d"
  done
}

# --- Flatpak mode ---------------------------------------------------------------
# The sandbox has no mounts or namespaces to give, but its "/", /dev and /sys
# are writable tmpfs dirs of its own. The partitions are composed on disk
# (hardlinks to the image cache, copies of the layers above it), and "/"
# gets symlinks to them; the broker shows the Android paths behind them
# (supervisor/broker/src/flat.rs).
if [[ $sandbox == flatpak ]]; then
  R=$root
  # A layer's files over the composed tree (cp unlinks a hardlinked file
  # before replacing it, so the image cache is never written).
  over() {
    if [[ -d $1 ]]; then cp -a --reflink=auto --remove-destination "$1/." "$2/"; fi
  }
  [[ -d $R ]] && chmod -R u+w "$R"
  rm -rf "$R"
  mkdir -p "$R"
  # Each partition's stack, bottom first (see the overlay stack below).
  for p in system vendor system_ext product odm system_dlkm vendor_dlkm odm_dlkm; do
    src=$img/$p
    [[ $p == system ]] && src=$img/system/system
    cp -al "$src" "$R/$p"
    for l in "$derived/common/$p" "$uid_layer/$p" "$android/$p" "${addon_layers[@]/%//$p}" "$run/layer/$p"; do
      over "$l" "$R/$p"
    done
  done
  cp -al "$apexdir" "$R/apex"
  over "$uid_layer/apex" "$R/apex"
  over "$run/layer/apex" "$R/apex"
  # Read-only files, as on read-only mounts: ART refuses to load a dex file
  # its process could write. (The hardlinked image cache is never written
  # either.) The uid mode's layer sets exact modes code verifies (clatd's
  # 06755), so it goes on again.
  find "$R" -type f -perm /222 -exec chmod a-w {} +
  over "$uid_layer/apex" "$R/apex"
  mkdir -p "$R/linuxapk/proc/sys/vm"
  ln -s /apex "$R/linuxapk/apex"
  sed -n 's/.*moduleName="\([^"]*\)" modulePath="\([^"]*\)".* versionCode="\([0-9]*\)".*/\2 \/apex\/\1@\3 erofs ro,fsoffset=0 0 0/p' \
    "$apexdir/apex-info-list.xml" >"$R/linuxapk/apexd-mounts"
  cp "$run/cmdline" "$run/bootconfig" "$R/linuxapk/proc/"
  echo 32 >"$R/linuxapk/proc/sys/vm/mmap_rnd_bits"
  # system_server needs a kernel config (see shim/); the sandbox can't read
  # /boot. The one option it reads, as every x86_64 distro kernel sets it.
  echo CONFIG_VMAP_STACK=y | gzip -c >"$R/linuxapk/config.gz"
  # No cgroup tree in the sandbox: /sys/fs/cgroup is a plain directory, so
  # process groups are bookkeeping only and kills fall back to the process
  # group (pgid). netd attaches its (emulated) eBPF programs there.
  mkdir -p /sys/fs/cgroup/system /sys/fs/cgroup/apps
  # The BPF sysctls netbpfload sets (linuxapk-bpfload binds the same).
  mkdir -p "$R/linuxapk/proc/sys/kernel" "$R/linuxapk/proc/sys/net/core"
  echo 2 >"$R/linuxapk/proc/sys/kernel/unprivileged_bpf_disabled"
  echo 1 >"$R/linuxapk/proc/sys/net/core/bpf_jit_enable"
  echo 0 >"$R/linuxapk/proc/sys/net/core/bpf_jit_kallsyms"

  for p in system vendor system_ext product odm system_dlkm vendor_dlkm odm_dlkm apex linuxapk; do
    ln -sfn "$R/$p" "/$p"
  done
  mkdir -p "$data"
  ln -sfn "$data" /data
  # vold bind-mounts /data/data onto /data/user/0, where apps' data dirs are
  # (the broker's lstat shows the symlink as that directory).
  mkdir -p "$data/user" "$data/data"
  [[ -L $data/user/0 ]] || ln -s /data/data "$data/user/0"
  # The image's other top-level entries, where the sandbox has none (its /etc
  # and /bin stay).
  for e in "$img/system"/* "$img/system"/.[!.]*; do
    [[ -e $e || -L $e ]] || continue
    n=$(basename "$e")
    [[ -e /$n || -L /$n ]] && continue
    if [[ -L $e ]]; then cp -P "$e" "/$n"; elif [[ -d $e ]]; then mkdir "/$n"; else cp -p "$e" "/$n"; fi
  done
  # Android's /etc is /system/etc; the sandbox's /etc stays (the glibc side
  # needs it) and gains the entries it lacks, such as task_profiles.json:
  # dirs as symlinks, files as copies (libbase opens files O_NOFOLLOW).
  for e in "$R/system/etc"/*; do
    n=$(basename "$e")
    [[ -e /etc/$n || -L /etc/$n ]] && continue
    if [[ -d $e && ! -L $e ]]; then ln -s "/system/etc/$n" "/etc/$n"; else cp -P -p "$e" "/etc/$n"; fi
  done
  binder_nodes /dev
  chmod 0700 "$run/bridge"
  mkdir -p /dev/socket
  ln -sfn "$run/bridge" /dev/socket/linuxapk
  ln -sfn "$run/kmsg" /dev/kmsg
  mkdir -p /sys/fs/bpf
  chmod 1777 /sys/fs/bpf
  start_init / LINUXAPK_FLATPAK=1 "LINUXAPK_ROOT_MAP=$R=/
$data=/data"
fi

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
    local IFS=: opts
    opts="lowerdir=${layers[*]}"
    # fsconfig(2) caps an option value at 256 bytes, which long checkout paths
    # (worktrees) exceed; "lowerdir+" (Linux 6.8+) adds one layer each.
    if ((${#opts} > 255)); then
      opts=$(printf 'lowerdir+=%s,' "${layers[@]}")
      opts=${opts%,}
    fi
    mount -t overlay overlay -o "ro,$opts" "$dst"
  fi
}

# --- partitions -------------------------------------------------------------
# Each partition's stack, top first: the start-time layer, the addons, our
# Android tree, the layers derived from the image, the image.
partition() { # name image-dir
  local p=$1 a layers=("$run/layer/$1")
  for a in "${addon_layers[@]}"; do layers+=("$a/$p"); done
  layer_mount "$R/$p" "${layers[@]}" "$android/$p" "$derived/common/$p" "$uid_layer/$p" "$2"
}
partition system "$img/system/system"
for p in vendor system_ext product odm; do
  partition "$p" "$img/$p"
done
for p in system_dlkm vendor_dlkm odm_dlkm; do
  layer_mount "$R/$p" "$img/$p"
done

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

# --- /apex: pre-flattened APEXes instead of apexd ---------------------------
# Init mounts an empty tmpfs over /apex early in second stage, so the tree is
# staged at /linuxapk/apex and init.rc bind-mounts it over /apex in
# early-init. It is one mount of the whole tree (no submounts), so
# non-recursive views of it stay complete.
# Our files for APEX contents (e.g. libtinyalsav2.so for
# com.android.hardware.audio, rc files) are in the start-time layer, for the
# APEXes this image has (linuxapk-session).
mkdir -p "$R/apex" "$R/bootstrap-apex"
layer_mount "$R/linuxapk/apex" "$run/layer/apex" "$uid_layer/apex" "$apexdir"
# For /system/bin/linuxapk-apexd: what apexd would see in /proc/mounts had it
# mounted each APEX itself (erofs file-backed: "<file> /apex/<name>@<ver>").
sed -n 's/.*moduleName="\([^"]*\)" modulePath="\([^"]*\)".* versionCode="\([0-9]*\)".*/\2 \/apex\/\1@\3 erofs ro,fsoffset=0 0 0/p' \
  "$apexdir/apex-info-list.xml" >"$R/linuxapk/apexd-mounts"

# --- writable state -----------------------------------------------------------
mkdir -p "$data"
mount --bind "$data" "$R/data"
# (The image has no cache partition: /cache -> /data/cache.)
for d in linkerconfig metadata mnt storage tmp data_mirror; do
  mkdir -p "$R/$d"
  mount -t tmpfs -o mode=0755 "$d" "$R/$d"
done

# --- /dev -------------------------------------------------------------------
mount -t tmpfs -o mode=0755,nosuid dev "$R/dev"
# udmabuf: dma-bufs from memfds for the gralloc HAL (logind ACL on the host).
for n in null zero full random urandom fuse udmabuf; do
  [[ -e /dev/$n ]] || continue
  touch "$R/dev/$n"
  mount --bind "/dev/$n" "$R/dev/$n"
done
# TUN (VPN apps, Thread networking): interfaces it creates live in the
# session's own network namespace. init.rc links /dev/net/tun -> ../tun.
if [[ -c /dev/net/tun ]]; then
  touch "$R/dev/tun"
  mount --bind /dev/net/tun "$R/dev/tun"
fi
# The desktop bridge's socket directory (the launcher started the bridge;
# it may restart and recreate its socket in there). Group audio (AID 1005,
# a subordinate gid on the host) can enter it: the audio HALs run as
# audioserver with that group.
if [[ $uid_mode == single ]]; then
  # Single mode: every Android process is uid 0 to the kernel.
  chmod 0700 "$run/bridge"
else
  chown 0:1005 "$run/bridge"
  chmod 0750 "$run/bridge"
fi
mkdir -p "$R/dev/socket/linuxapk"
mount --bind "$run/bridge" "$R/dev/socket/linuxapk"
# Android init logs to /dev/kmsg: hand it the host-side FIFO instead.
touch "$R/dev/kmsg"
mount --bind "$run/kmsg" "$R/dev/kmsg"
mkdir -p "$R/dev/pts" "$R/dev/socket"
# gid 5 (tty) isn't mapped in single mode.
pts_gid=5
[[ $uid_mode == single ]] && pts_gid=0
mount -t devpts -o newinstance,ptmxmode=0666,mode=0620,gid=$pts_gid devpts "$R/dev/pts"
ln -s pts/ptmx "$R/dev/ptmx"
if [[ -d /dev/dri ]]; then
  mkdir -p "$R/dev/dri"
  mount --rbind /dev/dri "$R/dev/dri"
fi
binder_nodes "$R/dev"

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
# Kernel config for code that wants /proc/config.gz (see shim/).
if [[ -r /boot/config-$(uname -r) ]]; then
  gzip -c "/boot/config-$(uname -r)" >"$R/linuxapk/config.gz"
fi

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
# emulator (supervisor/pivot-exec/bpfemu.c) are plain files here.
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
start_init "$R"
