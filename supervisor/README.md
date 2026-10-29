# supervisor

The syscall boundary between the host kernel and Android. One seccomp filter,
installed on Android's init and inherited by every Android process, turns what
an unprivileged namespace can't do into user notifications.
`mise run build:supervisor` builds all three artifacts.

| Dir          | Artifact (stage path)                                             | Runs                                                      | Start reading                 |
| ------------ | ----------------------------------------------------------------- | --------------------------------------------------------- | ----------------------------- |
| `pivot-exec` | `libexec/linuxapk/pivot-exec` (static C)                          | static pivot/namespace bootstrap and subreaper            | `pivot-exec.c`                |
| `broker`     | `share/linuxapk/android/system/bin/linuxapk-broker` (bionic Rust) | forked by pivot-exec in the sandbox, host-user privileges | `src/notify.rs`, `src/bpf.rs` |
| `shim`       | `share/linuxapk/android/system/lib64/liblinuxapk_shim.so`         | LD_PRELOADed into init and the services whose rc sets it  | `linuxapk_shim.c`             |

## What is trapped, and who serves it

| Syscalls                                                                                                                                      | Served by       | Why                                                                    |
| --------------------------------------------------------------------------------------------------------------------------------------------- | --------------- | ---------------------------------------------------------------------- |
| `openat` of `/dev/{binder,hwbinder,vndbinder}`, `ioctl` type `'b'`                                                                            | broker (binder) | binder in userspace: no binderfs, no SELinux module                    |
| `bpf`, `statfs` of `/sys/fs/bpf`, BPF-pin `getxattr`, `SO_ANDROID_DROP_REASON`, `RLIMIT_MEMLOCK`, alarm `timerfd_create`, RT scheduling       | broker (`bpf`)  | netd and the BPF loaders need eBPF; the rest fails in a user namespace |
| `get*id`/`set*id`, groups, the `chown` family, `SO_PEERCRED`, `lstat` of owner-tracked paths, `faccessat(W_OK)`, opening `/proc/<pid>/status` | broker (creds)  | virtual Android uids without a subordinate id range                    |
| `readlink`/`readlinkat`, `getcwd`, `fstat`, `mkdir`/`mkdirat`, the `chmod` family; `openat` of faked `/proc` files                            | broker (`flat`) | no mounts or user namespace in the sandbox (docs/session.md)           |
| `capget`, `capset`, the capability `prctl`s, `setsockopt(SO_MARK)`                                                                            | broker (`bpf`)  | no capabilities in the sandbox; the host's network                     |

The broker constructs and installs the filter (`src/filter.rs`) in its `--init`
entry point, hands the listener to pivot-exec, then execs Android. The outer
broker receives and serves every notification directly: binder, credentials,
Flatpak filesystem calls and BPF emulation share one Rust boundary. Each BPF
object retains one memfd inode; opening a pin reopens that inode with an
independent file description, so netd's OFD map locks conflict correctly.

pivot-exec stays static C because it runs before pivot_root, before Android's
bootstrap linker exists. It only establishes the root and namespaces, execs the
dynamic Rust broker and reaps children; it defines no syscall policy.

## Binder

`src/binder/driver.rs` follows `drivers/android/binder.c` and makes no syscalls,
so it is unit-tested on the host; `src/notify.rs` binds it to real processes:

- binder `openat` gets a 4 MiB memfd injected (`SECCOMP_IOCTL_NOTIF_ADDFD`),
  which the process maps as its receive area; the broker writes through its own
  mapping;
- ioctls reach the caller's memory through `/proc/<pid>/mem`, opened at binder
  open, which pins its address space;
- a read with nothing to do leaves its notification unanswered until there is
  work, and stays interruptible, so ptrace and job control work;
- polling threads get an eventfd in place of their binder fd;
- `sender_pid` is the pid in Android's namespace and `sender_euid` the opener's
  virtual;
- the freezer works as in the kernel, so ActivityManager freezes cached apps
  (`dumpsys activity` shows `use_freezer=true`): `BINDER_FREEZE` takes the pid
  in Android's namespace (the kernel's is the host's), and one that waits for
  its target to drain is answered when it drains or at its timeout
  (`notify.rs`'s monitor thread runs the timer). There is no cgroup freezer in
  the sandbox. `/dev/binderfs/features` (made by the launcher) tells libbinder
  the broker has freeze notifications, extended errors and oneway spam
  detection.

Not supported: priority inheritance, binder netlink reports and `TF_UPDATE_TXN`
(Android 17 doesn't send it). Two deviations from the kernel:

- `ADDFD` can't be undone, so if a transaction's later fd doesn't fit the
  reader's fd table, the fds already installed stay, and leak if the transaction
  then fails for good.
- A looper whose wait a signal ended still counts as idle until work wakes it
  and finds it gone (checking every waiter would cost a syscall per read). That
  delays a `BR_SPAWN_LOOPER` at most until the thread enters the ioctl again;
  only a cycle of nested callbacks that needs a new looper waits for its
  restart.

One broker thread serves the whole session, about 70k to 90k transactions per
second; a round trip costs 15 µs against 3.8 µs for kernel binder.

## Single-uid credentials

`src/creds/table.rs` is the pure model, `src/creds/syscalls.rs` the syscall
side. A process's identity is an id in its soft `RLIMIT_RTTIME`, which `fork`
inherits and `execve` keeps, so there is no fork tracking; the session has no
real-time tasks for the limit to affect. `set*id` follow the kernel's rules
applied to the virtual ids, and capabilities are virtual.

**The owner contract.** A `chown` runs as a no-op `fchownat(-1, -1)` (for the
kernel's errors and side effects) and records the owner as
`user.linuxapk.owner` = `"uid:gid"`. Readers:

- the broker's emulated `lstat` and `faccessat(W_OK)` for `/data`, the BPF pins,
  `/metadata` and clatd (a file without a record inherits its closest recorded
  ancestor's owner, so ART sees an app's APK as not writable by it);
- the shim's `stat` family in installd (`LINUXAPK_VSTAT=1` in its rc);
- `image/derive.sh single`, which writes the same xattr on image files whose
  owner Android verifies.

Not virtualized: `SCM_CREDENTIALS` (logd sees every writer as uid 0), owners of
files created without a `chown`, and vold's `/proc` scan.

## The shim

Hidden SELinux breaks call sites in init and a few services that assume a policy
(`security_compute_create`, `set*con`, `selinux_status_*`); the shim turns them
into no-ops. It also makes priority and rlimit changes best-effort, treats
vold's refused quota-project ioctl as success, skips POSIX ACL xattrs in
single-uid mode, and serves the staged `/proc/config.gz`.

In Flatpak mode the sandbox's own seccomp filter refuses some calls before ours
sees them, so the shim covers them where it is loaded: `mount`, `umount`,
`unshare`, `setns` and namespaced `clone` succeed as no-ops; `socket(AF_KEY)`
(which the connectivity stack only opens and closes) gets an eventfd; netd's
refused rtnetlink requests read as done (`LINUXAPK_NETLINK_ACCEPT`); vold's
uevent socket binds to a kernel-chosen port (concurrent sessions share the
host's network, so their pid-numbered ports collide); and lmkd's PSI trigger
windows are widened to 2 s (`LINUXAPK_PSI_WINDOW`). Init isn't pid 1 there, so
pivot-exec marks it with `LINUXAPK_SHIM_INIT`.

Android 17 mounts every visible emulated volume through FUSE; the old
`persist.sys.fuse` switch no longer selects a non-FUSE path. Flatpak has no
`/dev/fuse` and cannot mount filesystems. For vold only
(`LINUXAPK_EMULATED_STORAGE=1`), `shim/fuse.h` provides a socket with the stable
FUSE INIT request so MediaProvider starts its storage session. The launcher
links `/storage/emulated`, `/mnt/user/0/emulated` and
`/mnt/pass_through/0/emulated` to `/data/media`. All file access uses that same
filesystem, with no FUSE cache or filesystem requests. MediaProvider receives
the mounted-volume notification, scans the volume and serves MediaStore; apps
can index later file writes with MediaScanner or create them through MediaStore.
Unmount closes the socket and ends the session. This supports the primary
emulated volume, not AppFuse proxy descriptors or removable volumes.
Canonicalization keeps the public path, while cross-process `/proc` reads keep
the lower path so vold does not kill MediaProvider's recovery database on
unmount.

## Tests

- `cargo test -p linuxapk-broker` (binder transactions, failure delivery,
  freezing and credential behaviour) runs in `mise run test`.
- `tests/bpfemu.sh` and `tests/seccomp-filter.sh` (in `mise run test`) check the
  emulator's bounds and exactly which syscalls notify in each mode;
  `tests/shim-netlink.sh` the uevent port choice.
- `mise run test:binder` (`tests/binder/run.sh`) runs AOSP's binder gtests
  against the broker, then against kernel binder as the oracle (exit 77 when the
  host forbids binderfs; select this developer oracle with
  `LINUXAPK_BINDER_ORACLE=1`), in a root with the test unit's own cgroup for the
  freezer cases. The binaries come from
  `$XDG_DATA_HOME/linuxapk/test-bin/binder`, checked against
  `tests/binder/test-bin.sha256`; Android's CI doesn't publish them for the
  image's release, so they are built from AOSP (the header of `run.sh` says
  how). By default `tests/binder/flatpak.sh` runs them in a booted session
  instead, and exits 77 after them: there is no binderfs for the kernel oracle.
- `mise run test:e2e -- boot` boots and runs `selftest`.

How the supervisor fits the session: [docs/session.md](../docs/session.md); what
it may and may not defend against: [docs/security.md](../docs/security.md).
