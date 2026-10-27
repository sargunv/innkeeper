# ubinder: userspace binder (M4)

ubinder serves Android's binder IPC from a userspace broker instead of the
kernel driver. **It is the session's default** (since 2026-10-27): a session
needs **no binderfs mount and no SELinux module**. `start --kernel-binder` (or
`LINUXAPK_BINDER=kernel`) uses a private binderfs instead. Android is
unmodified; the session's seccomp supervisor intercepts the binder syscalls.

Status: at parity with kernel binder on AOSP's own binder tests, selftest, boot
and the compat corpus (headless and windowed). Latency is higher but doesn't
show at the system level (numbers below).

## How it works

`pivot-exec` installs one seccomp filter on Android init, inherited by every
Android process. It turns `bpf()` and friends, `openat()` and every `ioctl()`
with type `'b'` into user notifications. The kernel allows only one listener per
filter chain, so one process receives them all: the Rust broker helper
(`/system/bin/linuxapk-ubinder`, `ubinder/src/sys.rs`). It serves `openat` and
binder ioctls, and forwards the rest to the C bpf emulator thread
(`session/helper/bpfemu.c`) over a SOCK_SEQPACKET socket. Either side answers on
the shared listener.

- **open:** `openat` of `/dev/{binder,hwbinder,vndbinder}` (or the binderfs
  paths) gets a 4 MiB sparse memfd injected (`SECCOMP_IOCTL_NOTIF_ADDFD` with
  `SEND`). The process mmaps it as its receive region; the broker finds the
  mapping by inode in `/proc/<pid>/maps` and writes transactions through its own
  shared mapping. Other paths continue without taking the broker lock. ns-init
  creates readable dummy device nodes for libbinder's `access()` check.
- **ioctl:** served with `process_vm_{readv,writev}`. Errors are negative
  errnos, as from the driver.
- **blocking reads:** a `BINDER_WRITE_READ` with nothing to read is parked: the
  notification stays unanswered and the broker answers it when it wakes the
  thread. Waits stay interruptible, so ptrace (debuggerd) and job control work.
  A signal makes the kernel restart the ioctl, so restarts are idempotent:
  `write_consumed` is written back before parking, and read bytes whose answer
  failed (`ENOENT`) are stashed and returned by the thread's next read.
- **poll:** a thread that sets up polling (a write-only `BC_ENTER_LOOPER`, which
  is `IPCThreadState::setupPolling`) gets its process's binder fd swapped for an
  epoll fd (`ADDFD` with `SETFD`; the mapping keeps the memfd alive) watching an
  eventfd only the broker holds, so Android can't drain, saturate or block it.
  The broker sets it readable exactly when a polling thread has work
  (`binder_poll`). Nothing traps `epoll_ctl`.
- **fds:** `BINDER_TYPE_FD`/`FDA` are captured from the sender with
  `pidfd_getfd` and installed into the reader during its own read (`ADDFD`),
  like `binder_apply_fd_fixups`.
- **lifetime:** a monitor thread polls a pidfd per process (death), and every
  500 ms checks with `kcmp` that each process still holds its binder fd (close,
  exec). A lost fd releases the broker process (`binder_deferred_release`),
  which fires death notifications and fails pending calls.
- **identity:** `sender_pid` is the pid in Android's pid namespace (`NSpid`),
  `sender_euid` the opener's euid (its virtual one in single-uid mode, see
  [sandbox.md](sandbox.md)).
- **`linuxapk-session exec`:** nsenter'd processes don't descend from init, so
  they have no filter and no binder. In ubinder mode init runs `linuxapk-execd`
  (`session/helper/linuxapk-exec.c`), and `exec` runs commands through it: argv,
  env, cwd and stdio go over a 0600 socket on the session's `/dev`, like
  `adb shell`.

The broker (`ubinder/src/broker.rs`) is a pure state machine with no syscalls;
process access goes through a `Mem` trait. It follows `drivers/android/binder.c`
closely, and its comments name the kernel functions:

- per-process and per-thread work lists, with `process_todo` for deferred work;
- thread selection from `waiting_threads`; only looper threads take process
  work; `BR_SPAWN_LOOPER` replaces the leading `BR_NOOP`;
- transactions and buffers are separate objects, and a buffer owns the refs
  taken while translating it until `BC_FREE_BUFFER`;
- node refcounts and the INCREFS/ACQUIRE/RELEASE/DECREFS handshake with
  `BC_*_DONE`;
- the death-notification states, failed-reply chains, `return_error` and
  `reply_error`, per-node oneway queues, and async space with spam detection;
- `BINDER_GET_NODE_INFO_FOR_REF` (servicemanager's client callbacks) and
  extended errors.

Not supported: freezing. `BINDER_FREEZE` and `BINDER_GET_FROZEN_INFO` return
`EINVAL`, so Android disables its app freezer; freeze-notification commands are
accepted and never fire. Priority inheritance is ignored too.

## Proof

```sh
cargo test -p ubinder                                   # state machine + fuzz smoke
cargo run --release -p ubinder --bin ubinder-fuzz -- 250000 <seed>
ubinder/tests/run-binder-tests.sh [--ubinder] [TEST[:FILTER] ...]
LINUXAPK_INSTANCE=ubinder session/linuxapk-session start --ubinder --headless
```

- **Fuzzer:** 3 processes × 3 threads run random operations: grammar-built
  `BC_*` streams (transactions with object arrays, replies to received calls,
  refcounts, deaths, frees of real buffers), raw bytes, reads, thread exits and
  process deaths. Broker invariants are checked after every step, and once all
  processes are gone nothing may leak. 2M iterations (seeds 11–18): no panics.
  It caught a refcount underflow when a process died holding unfreed buffers.
- **AOSP binder tests** (android-17.0.0_r1 binaries, servicemanager running,
  rootless namespace; `run-binder-tests.sh`):

  | test                      | kernel binder           | ubinder                                                               |
  | ------------------------- | ----------------------- | --------------------------------------------------------------------- |
  | binderDriverInterfaceTest | 20/20                   | 17/17, 3 skipped (freeze notifications)                               |
  | binderLibTest             | 68 pass, 5 fail, 15.6 s | 69 pass, the same 5 fail, 15.5 s (+1: no freeze-notification support) |
  | binderSafeInterfaceTest   | 18/18                   | 18/18                                                                 |

  The 5 binderLibTest failures are environmental in both modes: netlink, RT
  scheduling, setuid and the SELinux access check.

- **Boot:** `sys.boot_completed` with 305 binder services and Launcher3 resumed.
  `selftest` 12/12. Warm boot (start to `boot_completed`, same data dir, three
  runs each): kernel 21.6–21.7 s, ubinder 22.2–22.3 s.
- **Compat corpus** (31 apps, CI image): headless 30/31 and `--window` (nested
  KWin) 30/31, the same as kernel binder. Only arm64-only Open Sudoku fails, as
  on kernel. One windowed StreetComplete double tap flaked on ubinder (3/6, 0/5
  on kernel). The cause was in the harness: its gesture events reached the input
  bridge in a burst before `app_process` was up, so the tap gap became injection
  timing (41 vs 55 ms, against GestureDetector's 40 ms minimum).
  `compat/gesture.sh` now waits for the bridge; the double tap then passed 4/4.
- **Latency**
  (`UBINDER_TEST_ARGS=... run-binder-tests.sh [--ubinder] binderThroughputTest`;
  median per transaction):

  | workload                          | kernel         | ubinder        |
  | --------------------------------- | -------------- | -------------- |
  | 1 client/server pair (`-w 2 -p`)  | 3.8 µs         | 15.0 µs        |
  | same, 16 KiB payload (`-s 16384`) | 3.6 µs         | 13.3 µs        |
  | 8 workers all-to-all (`-w 8`)     | 7.2 µs, 819k/s | 78.5 µs, 66k/s |

  The broker is one thread, so it tops out around 70–90k transactions/s
  session-wide. A desktop session uses a small fraction of that. If it ever
  matters, sharding by context, or by process with a lock per process, is next.

Tracing: `LINUXAPK_UBINDER_DEBUG=1` logs to the helper's stderr (`session.out`);
any other value is a log file path inside the session root (for example
`/data/ubinder.log`), rotated every 64 MiB.

## Hardening against hostile Android processes

The broker helper and the C supervisor run with the host user's power, so they
must survive anything Android sends. From a security review (2026-10-27):

- The receive memfd is sealed against resizing (a truncate made the broker's
  writes SIGBUS). Transaction sizes are aligned with checked arithmetic.
- A process's memory is read and written only through `/proc/<pid>/mem`, opened
  at binder open while its notification is valid. That pins its address space,
  so a thread killed by an exec, or a recycled tid, never gets writes meant for
  the old image. It costs about 1.5 µs per call (`binderThroughputTest -w 2 -p`
  median 11.5 → 13.3 µs, interleaved runs on one machine). Queued single-uid
  jobs are dropped once their notification is gone, and checked again before
  they act. Single-uid calls that transfer memory (`getresuid`, group lists,
  `SO_PEERCRED`, chown paths, lstat results) pin `/proc/<tid>/mem` the same way,
  per call; scalar `get*id` don't. Single-uid boot time is unchanged (fresh 4.87
  s, warm 2.60 s, and 0.86 s of receive-thread time in the first 10 s, before
  and after).
- Poll mode hands out an epoll fd, never the broker's eventfd (see "poll").
- The broker forwards to the C supervisor without waiting (`MSG_DONTWAIT`),
  failing the call with `EAGAIN` if it is behind, so binder never waits on it.
- `bpfemu.c`: element operations need a map with tables (not ring buffers), LPM
  keys and prefixes are bounded, and pins (and `statfs` of them) resolve with
  `openat2` beneath the session's `/sys/fs/bpf` tmpfs, which pivot-exec opens
  before Android runs, never through the caller's root, where Android could
  mount FUSE over it: no symlinks, no `..`, no other mounts, non-blocking, and
  regular files only. Objects are capped at 4096 and map memory at 128 MiB plus
  2 MiB per CPU (a boot creates about 250 objects and 25 MB on 32 CPUs), and
  failed creations are unwound. `session/tests/bpfemu.sh` covers these.

## Bugs found on the way (for the record)

- `binder_handle_cookie` is `__packed` (12 bytes). The old 16-byte struct gave
  `BC_REQUEST/CLEAR_DEATH_NOTIFICATION` the wrong command codes, so every
  `linkToDeath` failed with `EINVAL`. This was the boot stall.
- Errors went back as positive values (seccomp wants a negative errno), so
  failed ioctls looked successful.
- A `waiting_threads` list with stale entries suppressed `BR_SPAWN_LOOPER`.
  Non-looper threads took process work. Whole processes went non-blocking once
  one thread polled. `BC_*_DONE` never dropped the temporary node refs.
  `sender_pid` was the supervisor's pid, not Android's.
- Restarted ioctls replayed their write buffer. Killable waits would have made
  debuggerd's ptrace attach wait out its 30 s timeout on every crash.

## Why

Kernel mode needs a private binderfs per session and, on Fedora, a one-time
SELinux module so servicemanager can become the context manager.
[STRATEGY.md](STRATEGY.md) ("Packaging") sets M4's bar: binder with **no host
setup**, eventually inside sandboxes where nested user namespaces are blocked.
Virtual uids (no subuid range) can hang off the same broker: only how `euid` is
chosen at open changes.
