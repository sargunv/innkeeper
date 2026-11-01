# supervisor

The supervisor sits between the host kernel and Android. A seccomp filter is
installed on Android's init and inherited by every Android process. It turns
syscalls that an unprivileged namespace can't perform into user notifications,
which the broker serves. `mise run build:broker` builds the broker, and
`mise run build:ndk` builds the shim.

| Dir      | Artifact (stage path)                                       | Runs                                                     | Start reading                 |
| -------- | ----------------------------------------------------------- | -------------------------------------------------------- | ----------------------------- |
| `broker` | `libexec/innkeeper/innkeeper-brk` (static musl Rust)        | started by the launcher as init's parent                 | `src/notify.rs`, `src/bpf.rs` |
| `shim`   | `share/innkeeper/android/system/lib64/libinnkeeper_shim.so` | LD_PRELOADed into init and the services whose rc sets it | `innkeeper_shim.c`            |

## What is trapped, and who serves it

| Syscalls                                                                                                                                      | Served by       | Why                                                                        |
| --------------------------------------------------------------------------------------------------------------------------------------------- | --------------- | -------------------------------------------------------------------------- |
| `openat` of `/dev/{binder,hwbinder,vndbinder}`, `ioctl` type `'b'`                                                                            | broker (binder) | binder runs in userspace without binderfs or an SELinux module             |
| `bpf`, `statfs` of `/sys/fs/bpf`, BPF-pin `getxattr`, `SO_ANDROID_DROP_REASON`, `RLIMIT_MEMLOCK`, alarm `timerfd_create`, RT scheduling       | broker (`bpf`)  | netd and the BPF loaders need eBPF, and the rest fails in a user namespace |
| `get*id`/`set*id`, groups, the `chown` family, `SO_PEERCRED`, `lstat` of owner-tracked paths, `faccessat(W_OK)`, opening `/proc/<pid>/status` | broker (creds)  | Android's uids are virtual and share one kernel uid                        |
| `readlink`/`readlinkat`, `getcwd`, `fstat`, `mkdir`/`mkdirat`, the `chmod` family; `openat` of faked `/proc` files                            | broker (`fs`)   | the sandbox has no mounts or user namespace (docs/session.md)              |
| `capget`, `capset`, the capability `prctl`s, `setsockopt(SO_MARK)`                                                                            | broker (`bpf`)  | the sandbox has no capabilities and uses the host's network                |
| `openat` and `inotify_add_watch` of `/dev/input`, `ioctl` type `'E'`                                                                          | broker (input)  | the session's own evdev devices, which the bridge feeds                    |

The launcher runs `innkeeper-brk --preload <shim> /system/bin/init ...` in the
composed root. The broker forks a child, which installs the filter
(`src/filter.rs`), hands the listener back to the broker, and execs init with
the shim preloaded. The broker serves every notification, reaps orphans as the
subreaper, and exits with init's status. It stays an ancestor of every Android
process, so it can access process memory under Yama's default `ptrace_scope=1`
without capabilities or a sysctl override.

The `fs` notifications and the credential notifications for `chown`, `lstat` and
`access` share four worker threads. A slow lookup, such as an `fstat` through a
caller's fd, therefore can't block the binder receive thread. Each BPF object
keeps one memfd inode. Opening a pin reopens that inode with a separate file
description, so netd's OFD map locks conflict as they do on a kernel.

## Binder

`src/binder/driver.rs` follows `drivers/android/binder.c`. It makes no syscalls,
so it is unit-tested on the host. `src/notify.rs` connects it to real processes:

- A binder `openat` gets a 4 MiB memfd injected with
  `SECCOMP_IOCTL_NOTIF_ADDFD`. The process maps it as its receive area, and the
  broker writes to it through its own mapping.
- Ioctls reach the caller's memory through `/proc/<pid>/mem`. The broker opens
  this file when the process opens binder, which pins its address space.
- A read with no work leaves its notification unanswered until work arrives. The
  read stays interruptible, so ptrace and job control work.
- Polling threads get an eventfd in place of their binder fd.
- `sender_euid` is the opener's virtual euid.
- The freezer behaves as in the kernel, so ActivityManager freezes cached apps
  (`dumpsys activity` shows `use_freezer=true`). A `BINDER_FREEZE` that waits
  for its target to drain is answered when the target drains or when the timeout
  expires. The monitor thread in `notify.rs` runs the timer. The sandbox has no
  cgroup freezer.
- The launcher creates `/dev/binderfs/features`. It tells libbinder that the
  broker supports freeze notifications, extended errors and oneway spam
  detection.

The broker does not support priority inheritance, binder netlink reports or
`TF_UPDATE_TXN`. Android 17 doesn't send `TF_UPDATE_TXN`. The broker differs
from the kernel in two ways:

- `ADDFD` can't be undone. If a transaction's later fd doesn't fit in the
  reader's fd table, the fds already installed stay. They leak if the
  transaction then fails permanently.
- When a signal ends a looper's wait, the looper still counts as idle until work
  wakes it and finds it gone. Checking every waiter would cost a syscall per
  read. This delays a `BR_SPAWN_LOOPER` until the thread enters the ioctl again
  at the latest. The only case that waits for the restart is a cycle of nested
  callbacks that needs a new looper.

One broker thread serves binder for the whole session, at about 70k to 90k
transactions per second. A round trip takes 15 µs, compared with 3.8 µs for
kernel binder.

## Android credentials

`src/creds/table.rs` is the pure model, and `src/creds/syscalls.rs` handles the
syscalls. A process's identity is an id stored in its soft `RLIMIT_RTTIME`.
`fork` inherits the limit and `execve` keeps it, so the broker doesn't track
forks. The session has no real-time tasks for the limit to affect. The `set*id`
calls apply the kernel's rules to the virtual ids. Capabilities are also
virtual.

**The owner contract.** A `chown` runs as a no-op `fchownat(-1, -1)` so that the
kernel still returns its errors and side effects. The broker then records the
owner in the `user.innkeeper.owner` xattr as `"uid:gid"`. The record is read by:

- The broker's emulated `lstat` and `faccessat(W_OK)` for `/data`, the BPF pins,
  `/metadata` and clatd. A file without a record inherits the owner of its
  closest recorded ancestor. As a result, ART sees an app's APK as not writable
  by the app.
- The shim's `stat` family in installd.
- `image/derive.sh`, which writes the same xattr on image files whose owner
  Android verifies.

Three things are not virtualized: `SCM_CREDENTIALS`, the owners of files created
without a `chown`, and vold's `/proc` scan. Because of the first, logd sees
every writer as uid 0.

## The shim

SELinux is hidden in the sandbox. Init and a few services call functions that
assume a loaded policy (`security_compute_create`, `set*con`,
`selinux_status_*`), and the shim turns these calls into no-ops. The shim also
makes priority and rlimit changes best-effort, treats vold's refused
quota-project ioctl as a success, skips POSIX ACL xattrs, and serves the staged
`/proc/config.gz`.

Services that need the shim preload it from their override stanzas in
[innkeeper-services.rc](../android/overlay/system_ext/etc/init/innkeeper-services.rc).
These stanzas also set `INNKEEPER_SHIM_ROLE` to the service's name, which turns
on the service-specific workarounds below. Init isn't pid 1 in the sandbox, so
the broker gives it the role `init`.

The Flatpak sandbox's own seccomp filter refuses some calls before the
supervisor's filter sees them. The shim handles these calls in the processes
that load it:

- `mount`, `umount`, `unshare`, `setns` and namespaced `clone` succeed as
  no-ops.
- `socket(AF_KEY)` returns an eventfd. The connectivity stack only opens and
  closes this socket.
- netd's refused rtnetlink requests report success.
- vold's uevent socket binds to a port that the kernel chooses. Concurrent
  sessions share the host's network, so ports numbered by pid would collide.
- lmkd's PSI trigger windows are widened to 2 s.

lmkd writes Android scores from -1000 to 1000 to `/proc/<pid>/oom_score_adj`.
The shim maps that range onto the range from the session's inherited score to
1000, with a floor of 0. The kernel lets an unprivileged process lower a score
back to that floor, so a cached app that returns to the foreground gets its
relative protection back. lmkd keeps the original Android scores for its own
kill policy.

Android 17 mounts every visible emulated volume through FUSE. Flatpak has no
`/dev/fuse` and can't mount filesystems. For vold, `shim/fuse.h` provides a
socket that answers the stable FUSE INIT request, so MediaProvider starts its
storage session. The launcher links `/storage/emulated`, `/mnt/user/0/emulated`
and `/mnt/pass_through/0/emulated` to `/data/media`. All file access goes to
that filesystem directly, without a FUSE cache or FUSE requests. MediaProvider
receives the mounted-volume notification, scans the volume and serves
MediaStore. Apps can index files they write later with MediaScanner, or create
them through MediaStore. Unmounting closes the socket and ends the session. This
covers the primary emulated volume. AppFuse proxy descriptors and removable
volumes are not supported. Canonicalization returns the public path.
Cross-process `/proc` reads return the lower path, so vold does not kill
MediaProvider's recovery database on unmount.

## OpenHarmony

The launcher sets `INNKEEPER_GUEST` to the profile's system, `android` or
`ohos`, and the broker refuses to start without it. With `INNKEEPER_GUEST=ohos`
([ohos/](../ohos/README.md)), the broker keeps everything above and adds what
OpenHarmony's userspace expects from its kernel. The Android filter and driver
include none of these additions.

- **musl.** The broker is a static `x86_64-unknown-linux-musl` binary, so it
  runs in either guest's root, including OpenHarmony's, which has no Bionic.
  musl opens files with `open(2)`, so the filter also traps that call, and the
  broker serves it as `openat(AT_FDCWD, ...)`. Opens of `/etc/passwd` and
  `/etc/group` get the files from `/system/etc`. musl resolves service users
  there, and Flatpak binds its own files over both paths.
- **Binder.** OpenHarmony's kernel adds `BINDER_FEATURE_SET` for access tokens
  and sender info, plus `BINDER_GET_ACCESS_TOKEN` and `BINDER_GET_SENDER_INFO`.
  A transaction carries its sender's token and first token. The reading thread
  keeps them until its next read, together with the sender's pid for synchronous
  calls.
- **Access tokens** (`src/ohos.rs`). `/dev/access_token_id` is an empty file,
  and the broker serves its type-`'A'` ioctls. They cover each process's token
  and first token, which init or appspawn set before exec and which fork
  inherits. They also cover the kernel's per-token permission maps. Init fills
  these maps from the service cfgs, and the access-token service checks them.
  Records are keyed by pid and start time, so a reused pid starts from its
  parent's record. SPM entries and reference counts from newer kernels return
  `ENOTTY`, as on a kernel built without them.
- **Code signing** (`src/code_sign.rs`). `/dev/code_sign` is an empty file, and
  the broker serves its type-`'k'` ioctls. When installd installs a HAP with a
  debug signing profile, key_enable adds the profile's certificate chain there,
  and BMS refuses the install if that fails. The session's kernel enforces no
  code signatures, so the broker accepts each add and remove and keeps nothing.
- **Ashmem** (`src/ashmem.rs`). OpenHarmony's c_utils needs `/dev/ashmem`, and
  so does everything built on it, such as IPC parcels and the display HDI's
  command queues. Each open gets a new sealable memfd named `ashmem`. The
  broker's `fstat` reports it as a character device, which c_utils checks. The
  broker serves ashmem's ioctls on the memfd. The size is the memfd's size, a
  mask without `PROT_WRITE` becomes `F_SEAL_FUTURE_WRITE`, and pinning and
  purging do nothing because memfd pages are never purged. The state lives in
  the memfd, so a region passed to another process gives the same answers there.
- **Input devices in sysfs** (`src/input/sysfs.rs`). MMI's libinput gets each
  evdev device's udev properties from MMI's own libudev, which reads sysfs. The
  broker answers `stat`, `readlink`, `open` and `fstat` for the devices' nodes
  and sysfs entries. Their device numbers and `uevent` capability bitmaps match
  the devices' ioctls. A mouse's event node also carries the libinput property
  that real udev would add from its hwdb, a wheel click angle of 1 degree.
  musl's `stat(3)` is `stat(2)`, so the filter also traps that call.

## Tests

`mise run test` runs `cargo test -p innkeeper-broker`. This includes unit tests
for binder transactions, failure delivery, freezing and credentials. It also
includes `broker/tests/probes`, which drive the real filter and broker with real
processes. The probes check which syscalls the filter traps, the bpf emulator,
interrupted binder ioctls, the render-node policy and how the broker starts
init. `ctest --preset host-tests` runs `shim/tests`, which unit-test the shim's
headers.

- `mise run test:e2e -- boot` boots and runs `selftest`.
- `mise run test:e2e -- ohos` boots an OpenHarmony profile until SceneBoard is
  ready.

[docs/session.md](../docs/session.md) describes how the supervisor fits into the
session. [docs/security.md](../docs/security.md) describes what it defends
against and what it doesn't.
