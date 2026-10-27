# ubinder: userspace binder for no-privilege mode (M4)

Status: **milestone 1 reached** — the driver state machine is implemented in
Rust (`ubinder/`) and proven by unit tests and a fuzzer. The seccomp supervisor
wiring (openat/ioctl interception) and full-boot integration are the remaining
work. Kernel binder stays the default; ubinder changes nothing until it reaches
parity.

## Why

Kernel mode (M0–M3) needs a private binderfs per session and, on Fedora, a
one-time SELinux module so servicemanager can become the context manager.
[STRATEGY.md](STRATEGY.md) ("Packaging") sets M4's bar: binder with **no host
setup** — no binderfs mount, no SELinux module, and eventually inside sandboxes
where nested user namespaces are blocked. Virtual uids (no subuid range) hang
off the same broker. ubinder is a userspace reimplementation of the binder
driver that the session serves through the seccomp user-notification supervisor
it already runs for `bpf()` (see [m0-session.md](m0-session.md),
`session/helper/bpfemu.c`).

## Design

One broker per session reimplements the kernel driver's semantics. Android
processes are unmodified; the session's seccomp supervisor intercepts the binder
syscalls and routes them to the broker:

- **`openat` of `/dev/binder`, `/dev/hwbinder`, `/dev/vndbinder`** (and binderfs
  paths): the supervisor injects a **memfd** with `SECCOMP_IOCTL_NOTIF_ADDFD`.
  The process `mmap`s it; that becomes its receive-buffer region, exactly like
  the kernel's binder mmap. libbinder maps it `PROT_READ, MAP_PRIVATE`
  (`ProcessState.cpp`), never writes it, so the broker's writes through its own
  handle to the memfd are visible to the process. The broker learns each
  process's mmap base by scanning `/proc/<pid>/maps` for the injected memfd.
- **`ioctl` with a binder command** (`BINDER_WRITE_READ`, `BINDER_VERSION`,
  `BINDER_SET_MAX_THREADS`, `BINDER_SET_CONTEXT_MGR(_EXT)`,
  `BINDER_THREAD_EXIT`, freeze/extended-error/node-debug, …) is matched in the
  seccomp filter by the constant `cmd` (all share ioctl type `'b'`). The broker
  services it by reading and writing the caller's memory with
  `process_vm_{readv,writev}`.
- **`BINDER_WRITE_READ`**: the broker parses the `write` command stream
  (`BC_*`), performs object translation, and produces the `read` stream (`BR_*`)
  it copies back. Transaction payloads are written into the **target's** memfd
  region; the returned `data.ptr.buffer` is the target's mmap base + offset,
  reproducing the kernel's semantics.

### Blocking reads do not block the supervisor

A `BINDER_WRITE_READ` whose read half has no work yet must not stall the single
supervisor thread. The broker returns `blocked`; the supervisor stashes the
seccomp **notification id** against `(proc, tid)` and leaves it unanswered. Any
later call that enqueues work returns a list of `Wakeup{proc, tid}`; the
supervisor re-runs the read for each woken thread and answers the stored id with
`SECCOMP_IOCTL_NOTIF_SEND`. The kernel allows many outstanding notifications and
answering them from any thread, so one dispatch thread plus deferred responses
is enough. `SECCOMP_IOCTL_NOTIF_ID_VALID` guards against a target that died
while parked (its `NOTIF_SEND` returns `ENOENT`, ignored).

### fd translation

`BINDER_TYPE_FD`/`BINDER_TYPE_FDA` objects are **captured** out of the sender at
translate time with `pidfd_getfd` (so they survive the sender closing them) and
**installed** into the target at delivery time with `SECCOMP_IOCTL_NOTIF_ADDFD`
— which needs the target's outstanding notification, which we have because
delivery happens inside the target's own `BINDER_WRITE_READ`. This mirrors the
kernel's `binder_apply_fd_fixups`, which runs in the target's context. The
broker records fd fixups on the transaction and patches the recv buffer with the
new fd numbers just before emitting `BR_TRANSACTION`.

### Virtual uids

Each process has a broker-assigned `euid` reported as `sender_euid`. Today it is
the real caller uid (kernel-mode parity). In a later single-host-uid mode the
broker supplies the Android uid (system = 1000, apps 10000+) with no change to
the transaction path — only how `euid` is chosen at `open()`.

### Language

Rust, per the AGENTS.md policy: the broker parses untrusted data from every
Android process, so it is the archetypal security boundary. The state machine
(`broker.rs`) is `#![no]`-syscall — all process access goes through the `Mem`
trait — so it is fully host-testable and fuzzable. The syscall/seccomp layer is
a thin Rust shim (planned `sys.rs`) that implements `Mem` over
`process_vm_*`/`pidfd_getfd`/`ADDFD` and owns the dispatch loop. It integrates
with the existing C supervisor (`session/helper/`) by either linking the crate's
`staticlib` and calling it over a small C ABI, or running the binder dispatch on
its own seccomp listener thread; the C bpf supervisor is left untouched so
kernel binder stays the default. (Decision: keep the broker in Rust rather than
grow the C supervisor, because every byte it parses is hostile.)

## What is implemented and proven (milestone 1)

`ubinder/` (workspace crate) — the full driver state machine:

- contexts (binder/hwbinder/vndbinder) and the context manager
  (`BINDER_SET_CONTEXT_MGR(_EXT)`, uid check, handle-0 routing);
- per-process handle tables with smallest-free descriptor allocation (desc 0
  reserved for the context manager), strong/weak ref counts;
- node strong/weak refcounts and the `BR_INCREFS`/`ACQUIRE`/`RELEASE`/`DECREFS`
  handshake with `BC_*_DONE`, delivered as deferred thread work so it rides with
  the reply, as the kernel does;
- object translation inside transactions: `BINDER_TYPE_BINDER`/`HANDLE` (both
  directions, including handle→local-binder when the target owns the node),
  `BINDER_TYPE_FD`, `BINDER_TYPE_FDA`, and scatter-gather `BINDER_TYPE_PTR`
  buffers with `HAS_PARENT` pointer fixups (HIDL);
- synchronous reply routing and nested-call routing via a per-thread transaction
  stack, deferred `TRANSACTION_COMPLETE`;
- per-node oneway queue with async-space accounting and oneway-spam detection;
- death notifications (`BC_REQUEST/CLEAR_DEATH_NOTIFICATION`, `BR_DEAD_BINDER`,
  `BC_DEAD_BINDER_DONE`, and firing on process death);
- thread-pool growth (`BR_SPAWN_LOOPER`), `BINDER_SET_MAX_THREADS`, looper
  state, `BINDER_THREAD_EXIT`;
- `BINDER_VERSION` (8), extended error, node debug info; freeze ioctls accepted.

Proof:

```
cargo test -p ubinder      # 8 state-machine tests + fuzz smoke, all green
cargo clippy -p ubinder --all-targets   # clean under -D warnings (mise run check)
```

The tests drive the broker exactly as libbinder would (build a `BC_*` stream,
decode the `BR_*` stream) and assert the kernel's observable behaviour:
register-service + transact with a translated handle, sync call + reply routing,
fd passing (capture/install), oneway, the node ref handshake, and death
notification on peer death.

Fuzzer (`ubinder-fuzz`, no external deps, deterministic SplitMix64): feeds
random bytes as `write` buffers — the most attacker-controlled input — and
asserts the broker never panics and always drains or cleanly rejects the stream.

```
cargo run --release -p ubinder --bin ubinder-fuzz -- 500000 <seed>
# proven: 2,000,000 iterations across seeds 1..4, no panics
```

## Remaining work (gaps)

1. **`sys.rs` seccomp binding**: the `Mem` impl over `process_vm_*` + `pidfd` +
   `ADDFD`, `/proc/<pid>/maps` base discovery, and the dispatch/deferred-reply
   loop. Backward-compatible integration with `session/helper/` (the bpf
   supervisor must keep working; kernel binder stays default).
2. **Session flag** `linuxapk-session start --ubinder`: install the extended
   seccomp filter (trap `openat` on binder device paths and `ioctl` with type
   `'b'`), skip the binderfs mount in `init.rc`, and start the broker thread.
3. **Small integration test** (`ubinder/tests/run-ubinder.sh`, modelled on
   `hal/tests/run-android.sh`): the image's `servicemanager` plus a tiny NDK
   client/server under ubinder in a minimal namespace — register, lookup,
   transact, nested call, fd passing, death. The same binaries run against
   kernel binder for differential testing.
4. **Full boot** under `--ubinder` to `sys.boot_completed=1` with no binderfs,
   and the compat corpus (`compat/run.sh`) at parity with kernel binder. Measure
   binder throughput/latency (ping-pong) and boot time vs kernel binder.
5. **AOSP `binderLibTest`/`binderDriverInterfaceTest`**: not in the prebuilt
   image. Test hooks are stubbed so they run when the image-builder lane
   produces them; they are the differential oracle.
