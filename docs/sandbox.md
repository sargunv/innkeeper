# Sandboxed sessions: single-uid mode and relocation (M4)

M5 packages Linux.apk as Flatpaks. Inside a Flatpak sandbox a session can't rely
on what it needs today:

1. a subordinate uid range (`newuidmap`, `/etc/subuid`), so that Android's uids
   (system 1000, apps 10000 and up) are real host uids;
2. nested user namespaces (Flatpak's seccomp filter denies `CLONE_NEWUSER`);
3. mount namespaces for `/system`, `/apex`, `/data` and the rest.

This document covers the M4 work towards that. **Single-uid mode** removes the
first requirement and is prototyped and proven below. The **relocation
assessment** covers the other two and recommends a path for M5.

## Single-uid mode

```sh
session/linuxapk-session start --single-uid [--headless]   # or LINUXAPK_SINGLE_UID=1
```

The outer user namespace maps only uid and gid 0, to the invoking user, so no
`/etc/subuid` entry is needed. Every Android process is that one kernel uid. The
seccomp supervisor gives each process a **virtual** identity instead, the way
ubinder already serves binder. This needs ubinder (`--kernel-binder` is
refused), and it stays opt-in: normal (subuid) mode is the default until M5
decides.

### What's emulated

The session's seccomp filter (`session/helper/bpfemu.c`) also traps the
credential syscalls when `LINUXAPK_SINGLE_UID=1`, and the ubinder broker helper
answers them (`ubinder/src/vid.rs` is the pure model, `vidsys.rs` the syscall
side).

- **Where identities live.** Each process's credentials (real, effective, saved
  and fs uid and gid, and supplementary groups) are interned in a table. The
  process's soft `RLIMIT_RTTIME` holds the id. Rlimits are per process,
  inherited by `fork` and kept across `execve`, and the supervisor can read and
  set them with `prlimit(pid)`. So a new process carries its parent's identity
  with no fork tracking and no races. The default, `RLIM_INFINITY`, is root.
  `RLIMIT_RTTIME` only limits real-time tasks, and the session has none: its
  SCHED_FIFO/RR requests are already faked.
- **`get*id`, `getgroups`:** answer with the caller's virtual ids.
- **`set*id`, `setgroups`:** the kernel's rules from `kernel/sys.c` (`setuid`,
  `setreuid`, `setresuid`, `setfsuid` and the gid versions), applied to the
  virtual ids. "Privileged" means the caller's real effective `CAP_SETUID` or
  `CAP_SETGID`. `capset` runs natively, so capabilities that zygote and init
  drop stay dropped, and an app can't switch back to root.
- **binder:** `sender_euid` is the opener's virtual euid. The kernel also takes
  it from the opener's credentials at open time, and zygote children and init
  services open binder after they switch uid. `sender_pid` was already the pid
  in Android's namespace.
- **`SO_PEERCRED`:** the supervisor takes a duplicate of the socket
  (`pidfd_getfd`) and reads the real peer, then reports that peer's virtual euid
  and egid, with its pid in Android's namespace. A peer that's gone is reported
  as `AID_NOBODY`. This covers zygote, the property service, netd, lmkd, logd's
  control socket and `LocalSocket.getPeerCredentials`.
- **`chown` family:** an unmapped id would fail with `EINVAL`. The supervisor
  resolves the path in the caller's root (`openat2` with `RESOLVE_IN_ROOT`) and
  runs a no-op `fchownat(-1, -1)`. That keeps the kernel's errors (EROFS,
  ENOENT) and side effects (ctime, clearing set-id bits). It then records the
  owner in the `user.linuxapk.owner` xattr (`"uid:gid"`). These calls run on
  worker threads, so a path on a FUSE mount can't block the broker on a FUSE
  daemon that is itself waiting for binder.
- **Owners read back:** targeted, not a global `stat` trap.
  - The LD_PRELOAD shim's `stat`/`lstat`/`fstatat`/`fstat` report the recorded
    owner in installd, whose rc sets `LINUXAPK_VSTAT=1`. It costs one
    `getxattrat` per stat; the filter traps `getxattr` but not `getxattrat`. Not
    zygote: its children include WebView's sandboxed renderers, whose own
    seccomp policy kills them for `getxattrat`.
  - The supervisor emulates `lstat`, that is exactly
    `newfstatat(AT_FDCWD, path, st, AT_SYMLINK_NOFOLLOW)`, for paths that can
    carry an owner: `/data`, the eBPF pins, `/metadata` and clatd. Code that
    verifies owners mostly uses lstat (libcutils' `fs_prepare_dir_strict`,
    ClatCoordinator), and ClatCoordinator runs in the Tethering APEX's isolated
    linker namespace, which the shim doesn't reach. Every other lstat continues
    natively, and other stat calls aren't trapped.
- **`/proc/<pid>/status`:** opening it, or the `self`, `thread-self` and
  `task/<tid>` variants, gets a memfd with the real contents and the virtual
  `Uid`/`Gid`/`Groups` lines. ActivityManager checks that a content provider's
  process is still the same app with `Process.getUidForPid`. Without this every
  provider looked dead: apps waited 15 s for SettingsProvider, and the boot
  animation stopped 30 s late.
- **`access(W_OK)`** (`faccessat`/`faccessat2` asking for write access) by a
  non-root caller, for an owner-tracked path: checked against the recorded owner
  and the caller's virtual ids, with the kernel's permission-bit rules. A file
  with no record inherits its closest recorded ancestor's owner, so an app's own
  files belong to the app and APKs under `/data/app` belong to system. ART
  refuses to load a dex file the app can write. To the one kernel uid every APK
  is writable, so without this check every installed app failed to start
  ("Writable dex file ... is not allowed").
- **Other changes:**
  - The shim skips POSIX ACL xattrs, whose entries name unmapped ids. Without
    that, vold failed to prepare user storage and system_server rebooted to
    recovery to wipe `/data`.
  - `session/build.sh` makes host-owned copies of the files that normal mode
    gives subordinate owners (clatd and its directory), with their Android
    owners in the xattr (`out/session/overlay-su`).
  - devpts uses gid 0, and the bridge's socket directory is 0700.
  - The launcher refuses a data dir made in the other mode, because its owners
    don't carry over.

What isn't virtualized, and why that's acceptable:

- `SCM_CREDENTIALS`: trapping `recvmsg` would cost every socket read. logd
  therefore records every log writer as uid 0, so per-uid log filtering (an app
  reading its own logcat) and logd's chattiest-uid statistics don't work.
- The owners of files that apps create with `open(O_CREAT)` or `mkdir` without a
  `chown`. installd's own app directories are chowned, so its checks pass; the
  cache-clearing walks see such files as root's.
- vold's `/proc` scan for app processes (`stat` of `/proc/<pid>`), which
  remounts storage in app mount namespaces. With FUSE storage this doesn't
  matter.
- The desktop bridge sees every Android process as the invoking user, so its
  `SO_PEERCRED` allow list accepts them all. Inside a single-uid session that is
  no weaker than the session itself.

### Security: no isolation between apps

**In single-uid mode, apps are not isolated from each other.** Every Android
process is the same kernel uid, and permission bits are checked against that
uid, so any app can read or write any other app's files and signal its
processes. Android's own permission checks still run: binder, PackageManager and
AppOps see the right virtual uids. But they are policy enforced by the system's
own processes, and the kernel no longer backs them.

The boundary is the sandbox around the whole session, Flatpak in M5. That's the
same trust model as one desktop user running several native apps. Single-uid
mode is for running inside such a sandbox; normal mode stays the default on a
plain desktop.

Host safety is unchanged except for one point:

- Every host-facing guard still holds: the locked sysfs masks, the private
  network, IPC and UTS namespaces, the session's own cgroup, and no `/run/user`
  or `$HOME`.
- The host user's supplementary groups (for example `wheel`) can't be dropped
  without a subgid range: the kernel denies `setgroups` for an unprivileged
  single-id map.
- `selftest` therefore checks, in single-uid mode, that setgroups is denied,
  that those groups are unmapped inside, and that no host path bound into the
  session (`/data`, the device nodes, the kmsg FIFO, the bridge directory)
  belongs to one of them. A negative test (a file in `/data` with group `wheel`)
  makes it fail. Normal mode keeps the strict "no supplementary groups" probe.

### Proof

Same machine, CI image, headless. Boot time runs from `start` until
`getprop sys.boot_completed` returns 1, polled every 0.5 s.

|                                  | normal (subuid)       | single-uid                                     |
| -------------------------------- | --------------------- | ---------------------------------------------- |
| fresh `/data`                    | 4.89 s                | 4.90 s                                         |
| warm, 3 runs                     | 2.60, 2.60, 2.60 s    | 2.62, 2.58, 2.60 s                             |
| selftest                         | 12/12                 | 14/14 (with the single-uid group probes above) |
| input e2e (`android/e2e/e2e.sh`) | 27/27                 | 27/27                                          |
| compat (headless, 31 apps)       | 30/31, no regressions | 30/31, no regressions                          |

In both modes Open Sudoku is the one app that fails: it is arm64-only, as in the
baseline. Before the fixes above, single-uid compat was 2/31 (the writable-dex
check), then 28/31: WebView apps' renderers were killed by Chromium's seccomp
policy for the shim's `getxattrat`.

Single-uid overhead in the first 10 s of a fresh boot, from
`LINUXAPK_VID_STATS=1`:

- 187k `get*id` calls, 440 `set*id`, 7.3k chowns, 657 `SO_PEERCRED` lookups,
  6.1k trapped lstats (5.1k emulated) and 392 `status` opens;
- 0.86 s of time on the supervisor's receive thread.

A trapped `getuid` costs 8.3 µs, against 0.04 µs for an untrapped `getpid` (100k
calls in the session). `Process.myUid()` is the main caller. The calls are
spread over the parallel boot, so the boot-time difference is within noise.

Tests:

- `cargo test -p ubinder` covers the set*id rules, interning, the status rewrite
  and the path parsers, including randomized set*id sequences checked against
  the kernel's invariants and parser fuzzing.
- `session/tests/seccomp-filter.sh` installs the real filter in each mode and
  checks exactly which syscalls notify.

## Relocation assessment

What would it take to run with no user or mount namespace, that is inside
Flatpak? Uids are taken as solved by single-uid mode, so this is about paths and
mounts.

Sources:

- AOSP android-17.0.0_r1 (`~/aosp/a17`), the CI image, and Flatpak `main`
  (13b1eeb).
- Probes in Flatpak 1.18.4 and bwrap 0.12.0 (not setuid) on Fedora 44, kernel
  7.1.
- Hit counts are string literals beginning with `"/system`, `"/vendor`,
  `"/apex`, `"/data` and similar, excluding tests. Read them as orders of
  magnitude.

**The key finding.** Inside a Flatpak, `/`, `/dev` and `/sys` are tmpfs
directories owned by the user and writable by the app. bwrap mounts a tmpfs as
the new root, and Flatpak never remounts it read-only. `mkdir /system`,
`ln -s … /apex`, `mkdir /dev/socket` and `mkdir -p /sys/fs/bpf` all succeed.

So we can't mount at `/system`, but we can create `/system`, `/vendor`, `/apex`,
`/data` and the rest as **symlinks** into the image and the app's data
directory. The kernel then resolves `PT_INTERP`, `execve`, `open` and `stat`
through them with no traps at all.

### Linker

**`PT_INTERP`.** Executables use `/system/bin/linker64`, or
`/system/bin/bootstrap/linker64` for init and servicemanager. About 300 of them
are in the image.

**Running the linker explicitly** (`linker64 /abs/path`) works for one process
(`linker_main.cpp:256`), but not for its children:

- Every `execve` goes back through `PT_INTERP`: init (12 call sites), bionic
  libc (19: `system`, `popen`, `posix_spawn`), ART, vold, netd, installd,
  `crash_dump` and `ProcessBuilder`.
- `/proc/self/exe` would then name the linker.
- So it isn't viable as a general mechanism.

**Config selection.** The linker picks its config (`linker.cpp:3566-3610`):

- `/linkerconfig/<apex>/ld.config.txt` when the executable is under
  `/apex/<name>/bin/`;
- otherwise `/linkerconfig/ld.config.txt`;
- `LD_CONFIG_FILE` is debug-build only.

**Canonicalization.**

- The linker `realpath()`s the executable, the `dir.*` keys and `search.paths`.
- It compares each library's real path (`readlink /proc/self/fd/N`) against the
  namespace paths (`linker_namespaces.cpp:37-66`).
- Behind symlinks, those real paths become `/app/...`. That breaks APEX config
  selection, and `permitted.paths` checks for libraries `dlopen`ed by absolute
  path.
- The fix is for the supervisor to rewrite `readlinkat` results for
  `/proc/*/fd/*`, `/proc/*/exe` and `/proc/*/cwd` (plus `getcwd`) from the real
  prefix back to the Android one. These calls are much rarer than `stat`.

**linkerconfig** hardcodes `/system/${LIB}` and `/apex/<name>/${LIB}` (88
literals). Its `--root` option is host-only and changes only where inputs are
read from. With symlinks it runs unchanged and writes to a `/linkerconfig`
directory we create.

### ART and the framework

| Variable                                                     | Honoured by                                                      | Still hardcoded                                                                                                |
| ------------------------------------------------------------ | ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `ANDROID_ROOT`                                               | ART `GetAndroidRoot`, `Environment.java`, installd, nativeloader | `LocationIsOnSystemFramework` (`"/system"`: the hidden-API domain), nativeloader `/system/etc`, `/product/etc` |
| `ANDROID_ART_ROOT`, `ANDROID_APEX_ROOT`                      | `file_utils.cc`                                                  | boot image locations, nativeloader `kApexPath "/apex/"`                                                        |
| `ANDROID_DATA`                                               | ART, `Environment`, installd                                     | `is_global_cache = android_data == "/data"`, installd `/data/user`, `/data_mirror`                             |
| `ANDROID_I18N_ROOT`, `ANDROID_TZDATA_ROOT`                   | bionic tzcode, ICU                                               | bionic tries `/apex/com.android.tzdata/...` first                                                              |
| `BOOTCLASSPATH` and friends                                  | ART                                                              | the values are absolute `/apex` and `/system` paths set by init                                                |
| `VENDOR_ROOT`, `PRODUCT_ROOT`, `SYSTEM_EXT_ROOT`, `ODM_ROOT` | `Environment`, PackageManager's partition scan                   | nativeloader `kVendorLibPath`, `kAlwaysPermittedDirectories "/data:/mnt/expand"`                               |

The env vars cover the Java framework and most of ART. Native code still
hardcodes paths: nativeloader, odrefresh, artd and installd. Approximate counts
(read-only partitions / `/data`): ART about 45 / 17, framework Java and JNI
about 75 / 60.

### init

**Hardcoded paths** (134 / 22 literals in `system/core/init`, plus 548 rc lines
in the image):

- rc file locations;
- property files and `*_property_contexts`;
- `/dev/__properties__`, `/dev/socket/*` and `/dev/kmsg`;
- `/proc/cmdline` and `/proc/bootconfig`.

**Mounts** (33 calls outside first stage, 22 `mount` lines in our init.rc):

- `SetupMountNamespaces()` makes `/` shared and `/apex` and `/linkerconfig`
  private, bind-mounts the `/mnt` views, and may call `unshare(CLONE_NEWNS)`.
- A failure is fatal.

**Stubbing them.** Flatpak's filter returns EPERM for `mount`, `umount2`,
`unshare`, `setns`, `pivot_root` and `chroot`. ERRNO takes precedence over
USER_NOTIF, so the supervisor **cannot** make those calls succeed.

- The stubs belong in the shim, which init, zygote, vold and installd already
  preload: `mount`, `umount2`, `unshare(CLONE_NEWNS)` and `setns` return 0.
- The rc files drop the `/apex` and `/linkerconfig` tmpfs and bind mounts, the
  `/data_mirror` binds, cgroup2 and binderfs.
- The `/proc` fakes become memfd injection on `openat`, like
  `/proc/<pid>/status` above.

### Services and `/data`

| Component      | Hardcoded paths                                                                                                                                   |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| servicemanager | none of its own; libvintf has 30 (`"/apex"`, the vintf directories), with no override                                                             |
| libhardware    | `/system`, `/vendor`, `/odm`, `/apex/%s/...`                                                                                                      |
| apexd          | already bypassed by the pre-flattened tree; its wrapper's private mount namespace would switch to openat injection                                |
| vold           | mounts FUSE for `/storage`, impossible without `mount`; external storage would be a plain directory (unverified that A17 boots usefully that way) |

**`/data` is effectively fixed.** Apps see absolute `/data/user/0/...` paths and
store them in their own files, and init, vold, apexd, logd, netd and bionic all
assume `/data`. A symlink `/data` → `~/.var/app/<id>/data/android`, with the
readlink rewrite, keeps it.

### Effort

| Approach                                        | Work                                                                                                                                                                                                                                                                                                                                                    |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| True relocation (patch every path)              | about 2,000 lines across about 15 projects plus rc files. It needs our own AOSP build, abandons the unmodified CI image, and has a permanent rebase cost. **No.**                                                                                                                                                                                       |
| Path-rewriting seccomp layer (userspace chroot) | `openat`, `stat`, `statx`, `faccessat`, `readlinkat`, `mkdirat`, `unlinkat` and `renameat` can be emulated. `execve` with a relocated path, `chdir`, `AF_UNIX` `bind`/`connect` paths and `inotify` can't. Hot calls would cost a round trip each, which this document's measurements put at a few µs. A large, permanently incomplete surface. **No.** |
| ptrace (proot) or LD_PRELOAD                    | proot needs Flatpak's devel permission, doubles syscall cost and conflicts with debuggerd. LD_PRELOAD can't cover `PT_INTERP`, the linker's own opens, static binaries or raw syscalls. **No.**                                                                                                                                                         |
| **Symlink root**                                | symlinks created at launch, plus a `readlinkat`/`getcwd` rewrite, shim stubs for `mount`, `unshare` and `setns`, rc edits and `/proc` memfds. No AOSP patches. **Yes.**                                                                                                                                                                                 |

### Flatpak's constraints

Flatpak's filter (`common/flatpak-run.c`, `setup_seccomp`) allows by default and
blocks a list:

|                              | Syscalls                                                                                                                      |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Always EPERM                 | `unshare`, `setns`, `mount`, `umount`, `pivot_root`, `chroot`, `clone(CLONE_NEWUSER)`, keyrings                               |
| Always ENOSYS                | `clone3` and the new mount API                                                                                                |
| Devel permission only        | `ptrace`, `perf_event_open`, `personality`                                                                                    |
| Allowed (confirmed by probe) | `seccomp(NEW_LISTENER)` and the notification ioctls, `process_vm_readv`/`writev`, `pidfd_open`, `pidfd_getfd`, `memfd_create` |

`NoNewPrivs` is 1 and there are no capabilities. **The supervisor works inside
Flatpak.**

**Sub-sandboxes.** The Spawn portal and `flatpak-spawn --sandbox` only start a
tighter instance of the same app:

- exposed paths keep their own locations;
- the only relocations are `/app` and `/usr`;
- each instance still gets a writable root tmpfs (tested).

`--device=all` dev-binds the host `/dev`, which loses the writable `/dev`, so
request `--device=dri` only.

**What Flatpak provides for free:**

- `/sys` is only read-only binds of `block`, `bus`, `class`, `dev` and
  `devices`, so there's no fusectl and no selinuxfs;
- there are no capabilities;
- each app gets its own systemd scope.

**What it doesn't provide:**

- a private network namespace: it offers host network or none, so pasta isn't
  possible, Android would bind ports on the host network, and
  ConnectivityService needs some other network agent;
- cgroup delegation;
- FUSE mounts.

**Other ways to get the mounts:**

- **`flatpak-spawn --host`** needs `--talk-name=org.freedesktop.Flatpak`, which
  is a full sandbox escape and unlikely to pass Flathub review.
- **Waydroid** uses a root LXC system service instead and isn't on Flathub.
- **setuid bwrap** support was removed upstream in 0.12.0.
- **Ubuntu 24.04** restricts unprivileged user namespaces through AppArmor, so a
  native launcher there needs a profile with `userns,`.

## Recommendation for M5

1. **Prototype first: a symlink root inside Flatpak, with no AOSP patches.**
   - **How:**
     - The build bakes the composed image into `/app/android/...`, or a shared
       extension, with the overlays applied at build time because overlayfs is
       unavailable.
     - The launcher creates the top-level symlinks and directories on the
       writable root.
     - The session runs in single-uid mode with no user namespace at all: the
       real uid is the user's, and the virtual root is uid 0.
     - The supervisor adds `readlinkat`/`getcwd` rewriting and `/proc` memfds.
     - The shim stubs `mount`, `unshare` and `setns`, and the rc files drop
       their mounts.
   - **Pros:**
     - The image stays unmodified, with no AOSP patches.
     - Nothing on the hot path is trapped.
     - Flatpak is the security boundary that single-uid mode needs.
     - Per-app bundles work.
   - **Cons:**
     - It relies on undocumented Flatpak behaviour (the writable root).
     - It needs a new network design, with no network namespace.
     - There's no FUSE storage and no cgroups.
     - EPERM precedence over the supervisor is verified only on kernel 7.1.
   - **Go/no-go:** `sys.boot_completed` inside `flatpak run`, and an app with
     networking.
2. **Fallback and power-user path: the session as a native package, with a
   Flatpak UI.**
   - Ship today's launcher as an RPM or deb, a systemd-sysext, or a user
     service.
   - All host-safety invariants stay as they are: the sysfs masks, pasta and the
     session's own cgroup.
   - Ubuntu needs an AppArmor `userns` profile.
   - The Flatpak UI reaches the session through a socket (`xdg-run/linuxapk`),
     the libvirt pattern.
   - Single-uid mode still removes the subuid requirement there.
3. **Avoid: a host helper reached through `flatpak-spawn --host`.**
   - It's a sandbox escape that Flathub is unlikely to accept.
   - It still needs host user namespaces.
   - Option 2 gives the same capabilities with an honest install story.
