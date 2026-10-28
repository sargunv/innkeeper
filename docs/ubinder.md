# ubinder: userspace binder (M4)

> **Status (2026-10-28):** the cross-process broker stays, simplified for
> correctness and clarity: the driver owns every thread's place in an ioctl, and
> a read is committed only once the reader has it (see "The read lifecycle").
> Guards that only defended against deliberately malicious callers are gone (the
> S11 page-protection checks, the S12 fallback, the sealed receive memfd and the
> epoll indirection for poll), so such a process can now take binder down for
> the whole session (see "Bounds and process pinning"), as a desktop app can
> kill the user's session. See [STRATEGY.md](STRATEGY.md) "Threat model".

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
(`/system/bin/linuxapk-broker`, `supervisor/broker/src/notify.rs`). It serves
`openat` and binder ioctls, and forwards the rest to the C bpf emulator thread
(`supervisor/pivot-exec/bpfemu.c`) over a SOCK_SEQPACKET socket. Either side
answers on the shared listener.

- **open:** `openat` of `/dev/{binder,hwbinder,vndbinder}` gets a 4 MiB sparse
  memfd injected (`SECCOMP_IOCTL_NOTIF_ADDFD` with `SEND`). The process mmaps it
  as its receive region; the broker finds the mapping by inode in
  `/proc/<pid>/maps` and writes transactions through its own shared mapping.
  Other paths continue without taking the broker lock. ns-init creates readable
  dummy device nodes for libbinder's `access()` check.
- **ioctl:** served through the opener's `/proc/<pid>/mem`, pinned at open (see
  "Bounds and process pinning"). Errors are negative errnos, as from the driver.
- **blocking reads:** a `BINDER_WRITE_READ` with nothing to read blocks: the
  notification stays unanswered and the driver answers it once there is work.
  Waits stay interruptible, so ptrace (debuggerd) and job control work. A signal
  makes the kernel restart the ioctl, which is idempotent (see "The read
  lifecycle").
- **poll:** when a thread sets up polling (a write-only `BC_ENTER_LOOPER`, which
  is `IPCThreadState::setupPolling`), its process's binder fd is swapped for an
  eventfd (`ADDFD` with `SETFD`; the mapping keeps the memfd alive), once per
  process and before the write is consumed, so a restarted ioctl retries a swap
  that failed. The broker sets it readable exactly when a polling thread has
  work (`binder_poll`). Nothing traps `epoll_ctl`.
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

The driver (`supervisor/broker/src/binder/driver.rs`) is a state machine that
makes no syscalls: process memory, receive buffers, fds and answering a call go
through its `Userspace` trait, which `notify.rs` implements over seccomp and the
tests over an in-memory fake (`src/fake.rs`). It follows
`drivers/android/binder.c` closely, and its comments name the kernel functions:

- per-process and per-thread work lists, with `process_todo` for deferred work;
- thread selection: the longest-blocked thread that takes process work; only
  looper threads do; `BR_SPAWN_LOOPER` replaces the leading `BR_NOOP`;
- transactions and buffers are separate objects, and a buffer owns the refs
  taken while translating it until `BC_FREE_BUFFER`;
- node refcounts and the INCREFS/ACQUIRE/RELEASE/DECREFS handshake with
  `BC_*_DONE`;
- the death-notification states, failed-reply chains, `return_error` and
  `reply_error`, per-node oneway queues, and async space with spam detection;
- `BINDER_GET_NODE_INFO_FOR_REF` (servicemanager's client callbacks) and
  extended errors.

### The read lifecycle

The driver serves whole ioctls (`BinderDriver::ioctl`) and is the only owner of
a thread's state in them: the seccomp layer keeps no waiters and no stash. Each
read goes through one lifecycle:

- **prepared:** the read takes work off its lists and renders the `BR_*` stream,
  changing nothing else yet;
- **blocked:** with nothing to read, the thread waits holding its unanswered
  call. Work for it wakes it, and so does process work, which goes on the
  process's list and wakes the blocked looper that waited longest; the woken
  read runs again before the ioctl that woke it returns;
- **committed:** once the stream and the bwr are copied out and the call is
  answered, the delivery takes effect (transactions go on the reader's stack,
  buffers become the reader's, node and death states advance);
- **rolled back:** if a copy fails (`EFAULT`) or the call is gone (a signal),
  the work goes back to the front of the lists it came from: thread work for the
  thread's restarted read, process work for the next looper that reads;
- **abandoned:** a wait ends without a read when the thread issues another
  ioctl, when it is woken but its call is gone (a signal stopped it, or it will
  restart: nothing is written to it, and its process work goes to the next
  looper), or when process release answers it with `EBADF`.

A write is consumed as it is processed and its progress written back whatever
the read does, before blocking too, so a restarted ioctl never replays it. Fds
installed for a rolled-back transaction stay installed and their numbers stay in
the buffer, so a reread reports the same fds. One deviation: the kernel installs
all of a transaction's fds or none, but `ADDFD` can't be undone, so if a later
fd doesn't fit the reader's fd table, the ones already installed stay, and leak
if the transaction fails for good (it fails when it is the first item of a read:
a oneway is dropped, a call gets `BR_FAILED_REPLY`, and so does the reader of a
reply). Another: a looper whose wait a signal ended still counts as idle until
work wakes it and finds it gone, which can delay a `BR_SPAWN_LOOPER`; checking
every waiter would cost a syscall on every read. `src/binder/lifecycle_tests.rs`
drives libbinder-like threads through every failure point, stopped threads and
thread exits, and checks that no work waits with a thread that isn't reading it,
every synchronous call gets its reply or a failed reply, every oneway sent is
received, no ended call's memory is written, and nothing leaks.

Not supported: freezing. `BINDER_FREEZE` and `BINDER_GET_FROZEN_INFO` return
`EINVAL`, so Android disables its app freezer; freeze-notification commands are
accepted and never fire. Priority inheritance is ignored too.

## Proof

```sh
cargo test -p linuxapk-broker                    # driver, lifecycle properties, fuzz smoke
cargo run --release -p linuxapk-broker --bin linuxapk-broker-fuzz -- 500000 <seed>
supervisor/tests/binder/run.sh [TEST[:FILTER] ...]  # broker, then kernel (mise run test:binder)
mise run test:boot                               # fresh headless boot + selftest, both uid modes
```

- **Fuzzer:** 3 processes × 3 threads run random operations: grammar-built
  `BC_*` streams (transactions with object arrays, replies to received calls,
  refcounts, deaths, frees of real buffers), raw bytes, reads, thread exits,
  process deaths, and signals that interrupt blocked reads, which then restart.
  In most iterations userspace also fails at random under the driver: copies,
  receive allocations, fd captures and installs, and answers. Driver invariants
  are checked after every step, and once all processes are gone nothing may leak
  (transactions, buffers, nodes, captured fds, receive space). 500k iterations
  (seed 1): no panics. It caught a refcount underflow when a process died
  holding unfreed buffers.
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
- **Latency** (`UBINDER_TEST_ARGS=... run-binder-tests.sh binderThroughputTest`;
  median per transaction):

  | workload                          | kernel         | ubinder        |
  | --------------------------------- | -------------- | -------------- |
  | 1 client/server pair (`-w 2 -p`)  | 3.8 µs         | 15.0 µs        |
  | same, 16 KiB payload (`-s 16384`) | 3.6 µs         | 13.3 µs        |
  | 8 workers all-to-all (`-w 8`)     | 7.2 µs, 819k/s | 78.5 µs, 66k/s |

  The read-lifecycle rewrite (2026-10-28) kept these: four interleaved runs each
  against the broker before it, on a loaded machine, gave medians of 69.4k/s and
  74 µs against 65.4k/s and 80 µs for `-w 8`, 17.0 against 17.3 µs for 16 KiB
  payloads, and 18 against 20 µs for one pair (both noisy).

  Not writing to a thread whose wait a signal ended costs a
  `SECCOMP_IOCTL_NOTIF_ID_VALID` per woken read. Against the broker before that
  (2026-10-28, six interleaved runs each, load average 20–40): one pair 110k/s
  and 13.1 µs against 107k/s and 13.4 µs, 16 KiB payloads 68.6k/s against
  64.7k/s, and `-w 8` 60.3k/s against 52.1k/s in one round but 31.9k/s against
  31.5k/s in the next, so within the noise there.

  The broker is one thread, so it tops out around 70–90k transactions/s
  session-wide. A desktop session uses a small fraction of that. If it ever
  matters, sharding by context, or by process with a lock per process, is next.

Tracing: `LINUXAPK_BROKER_DEBUG=1` logs to the helper's stderr (`session.out`);
any other value is a log file path inside the session root (for example
`/data/broker.log`), rotated every 64 MiB.

## Bounds and process pinning

The broker helper and the C supervisor run with the host user's power. Under
desktop parity they keep simple bounds and correctness checks, and never write
into the wrong process; they don't defend against deliberately malicious
callers. From a security review (2026-10-27), what remains:

- Transaction sizes are aligned with checked arithmetic.
- A process's memory is read and written only through its `/proc/<pid>/mem`,
  opened at binder open before its notification is validated. That pins its
  address space, so a thread killed by an exec, or a recycled tid, never gets
  writes meant for the old image. `mem` uses FOLL_FORCE, so it also reaches the
  caller's read-only and `PROT_NONE` private pages, where `process_vm_*` and the
  driver fail with `EFAULT`. Under desktop parity that is fine: it is the
  caller's own memory. The page-protection checks that used to prevent it
  (`PROCMAP_QUERY`, or the maps text before Linux 6.11) were removed; that
  raised `binderThroughputTest -w 8` all-to-all from 55.5–58.1k to 63.0–68.8k
  transactions/s, median per transaction 90–95 µs to 76–84 µs (six interleaved
  runs each on one loaded machine). A bwr that can't be written back fails the
  ioctl with `EFAULT`, as in the driver, and the read it carried is rolled back
  rather than lost. Queued single-uid jobs are dropped once their notification
  is gone, and checked again before they act. Single-uid calls that transfer
  memory (`getresuid`, group lists, `SO_PEERCRED`, chown paths, lstat results)
  pin the same way, per call; scalar `get*id` don't.
- What a misbehaving process can do to the broker, accepted under desktop
  parity: it can resize its receive memfd, and a shrink makes the broker's next
  write through its own mapping fault (SIGBUS, which kills the broker), and it
  shares its poll eventfd's open file with the broker, so if it clears
  `O_NONBLOCK` and drains or fills the counter, the broker's next read or write
  of it blocks under the broker's lock. Either way binder stops for the whole
  session. Sealing the memfd and an epoll fd in front of the eventfd prevented
  both; they were dropped as complexity serving only that case.
- The broker forwards to the C supervisor without waiting (`MSG_DONTWAIT`),
  failing the call with `EAGAIN` if it is behind, so binder never waits on it.
- `bpfemu.c`: element operations need a map with tables (not ring buffers), LPM
  keys and prefixes are bounded, and pins (and `statfs` of them) resolve with
  `openat2` beneath the session's `/sys/fs/bpf` tmpfs, which pivot-exec opens
  before Android runs, never through the caller's root, where Android could
  mount FUSE over it: no symlinks, no `..`, no other mounts, non-blocking, and
  regular files only. Objects are capped at 4096 and map memory at 128 MiB plus
  2 MiB per CPU (a boot creates about 250 objects and 25 MB on 32 CPUs), and
  failed creations are unwound. `supervisor/tests/bpfemu.sh` covers these.

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
