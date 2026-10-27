# The desktop bridge (`linuxapk-bridge`)

A host-side Rust daemon (`bridge/`) that gives the Android session access to the
desktop: audio now, and later display, notifications, portals, MPRIS and the
clipboard. It runs on the host as the user (glibc, outside the namespaces).
Android-side clients reach it over one Unix socket.

The bridge is a **security boundary**. Everything it reads from a client is
untrusted: message bodies, shared memory, and fds the client may still hold.

## Running

`session/linuxapk-session start` starts it (from `target/release`, building it
first if needed). It runs inside the session's systemd unit, so `stop` ends it
and it shares the unit's memory cap. If it exits, a shell loop restarts it.

- **Socket:** `out/run[-<instance>]/bridge/bridge.sock` on the host. The
  directory is bind-mounted into the session as `/dev/socket/linuxapk`
  (`root:audio 0750`, set up by `ns-init.sh`). The socket itself is 0666, so the
  directory is the access control: only `audio` group members can enter it. A
  restarted bridge recreates the socket in the same bound directory, and clients
  reconnect.
- **Allow-list:** the bridge also checks `SO_PEERCRED` and only accepts the host
  uids of Android root (the host user), `system` (1000) and `audioserver`
  (1041), which are mapped onto the subordinate range.
- **Log:** `out/run[-<instance>]/bridge.log`. Set `LINUXAPK_BRIDGE_VERBOSE=1`
  for per-stream statistics every 5 s.
- **Environment:**
  - `LINUXAPK_AUDIO_SINK=<sink name>` routes the session's audio to one PipeWire
    sink (the tests use a null sink). By default it goes to the desktop's
    default output.
  - Standalone:
    `linuxapk-bridge --socket PATH [--allow-uid UID]... [--sink NAME] [--null] [-v]`.
- **Tests:** `cargo test -p linuxapk-bridge` covers the protocol, the ring, a
  live server with hostile clients, and proptest fuzzing of every decoder.
  `cargo run -p linuxapk-bridge --example tone -- SOCKET` plays a tone the way
  the Android client does.

## Code layout and conventions for new services

| Module      | What it is                                                                                       |
| ----------- | ------------------------------------------------------------------------------------------------ |
| `wire`      | Framing plus the untrusted-input helpers: `Reader`, `Writer`, `decode_header`, `reply`, `errno`  |
| `transport` | The `SOCK_SEQPACKET` socket: `listen`, `accept` (with `SO_PEERCRED`), `recv`/`send` with fds     |
| `proto`     | Protocol versions, `HELLO`, service ids                                                          |
| `server`    | Accept loop, HELLO, the `Service` trait, `Registry`, client and uid caps                         |
| `audio`     | The audio service: `proto` (its messages), `ring`, `stream` (threads), `pulse` (PipeWire output) |

To add a service (the windowing lane's display service is id 2, types
`0x0200..=0x02ff`):

1. **Id and types.** Take a service id in `proto::service` and a type range
   `0xNN00..=0xNNff` (NN = the id). Requests live below `0x4000`; events (bridge
   to client, tag 0) live in `0x4000..=0x7fff`.
2. **Messages.** Write `<service>/proto.rs`, with `decode(ty, body)` built only
   on `wire::Reader` and finishing with `Reader::end()`, plus `encode(...)` for
   tests. Rules:
   - Bound every count and length.
   - Names go through `Reader::name` (bounded UTF-8, no control characters).
   - Floats go through `unit_f32` or an explicit range check.
   - Reserved fields must be 0.
   - Add proptests: arbitrary bodies never panic, and anything accepted
     re-encodes to the same bytes.
3. **The service.** Implement `server::Service::handle(ty, body, fds)`. It runs
   on the connection's thread, so keep it short and hand long work to your own
   threads. Close fds you don't expect: drop them and return `-EPROTO`. Check
   fds you do accept (`fstat`, `F_GET_SEALS`, size) before you map them.
   Dropping the service is the connection's cleanup. Put per- client caps
   (memory, objects) in the service, as `audio::Limits` does.
4. **Registration.** Register a factory with
   `Registry::register(id, name, |peer| ...)` in `main.rs`. If the service has
   new clients, extend the launcher's `--allow-uid` list and document them
   below.
5. **Shared memory and fds you hand out.** Create them in the bridge, and seal
   memfds against shrinking (`F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`) so a
   client can't truncate them under your mapping and SIGBUS the bridge. Use
   non-blocking eventfds on both sides. Never read back state you own from
   shared memory; keep it in the bridge and mirror it out. Clamp whatever the
   client writes there.

## Protocol v1

Transport: `AF_UNIX`, `SOCK_SEQPACKET`. One packet is one message. All integers
are little-endian.

```
header (8 bytes)    u16 type, u16 flags (must be 0), u32 tag
body                type-specific, at most 504 bytes (whole message <= 512)
name                u16 len (<= 96) + UTF-8 bytes, no NUL or control chars
reply               type 0x8000, the request's tag, i32 status (0 or -errno), body
```

- Every request gets exactly one reply, in order. A reply may carry fds
  (`SCM_RIGHTS`, at most 4).
- No request in v1 carries fds. If one arrives with fds, the bridge closes them
  and replies `-EPROTO`.
- **Errors:**
  - A malformed body gets `-EINVAL`, and an unknown type gets `-ENOSYS`. Either
    way the connection stays open.
  - An unreadable header, a packet over 512 bytes or more than 4 fds ends the
    connection.
- **Versioning:** minor versions only add message types or trailing reply
  fields. Clients must ignore reply bytes they don't know. A new major version
  means a new HELLO handshake.

### Core

| Type     | Message | Body                                                  | Reply body               |
| -------- | ------- | ----------------------------------------------------- | ------------------------ |
| `0x0001` | HELLO   | `u16 major (1)`, `u16 minor (0)`, `u32 service`, name | `u16 major`, `u16 minor` |

HELLO must be the first message and is sent once. It binds the connection to one
service; a client that needs two services opens two connections.

- A wrong major gets `-EPROTO`, and an unknown service gets `-ENOSYS`. Either
  way the connection is closed.
- Any other first message gets `-EPROTO` and also closes it.
- Service ids: 1 audio, 2 display (reserved).

### Audio service (id 1)

| Type     | Message      | Body                                                                                                                                               | Reply                                                                                                                                  |
| -------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `0x0101` | OPEN_OUTPUT  | `u32 rate` (8000–192000), `u8 channels` (1–2), `u8 format` (1 s16le, 2 f32le), `u16 0`, `u32 period_frames` (16–16384), `u32 periods` (2–16), name | `u32 stream`, `u32 capacity_frames`, `u32 data_offset`, `u32 map_size`, `u32 latency_us`; fds: ring memfd, data eventfd, space eventfd |
| `0x0102` | CLOSE        | `u32 stream`                                                                                                                                       | (none)                                                                                                                                 |
| `0x0103` | STANDBY      | `u32 stream`                                                                                                                                       | (none): once the ring drains and PipeWire underruns, the stream is corked so the sink can suspend                                      |
| `0x0104` | SET_VOLUME   | `u32 stream`, `f32 left`, `f32 right` (each 0–1)                                                                                                   | (none)                                                                                                                                 |
| `0x0105` | SET_MASTER   | `f32 volume` (0–1), `u8 muted` (0/1); applies to all of the connection's streams, current and future                                               | (none)                                                                                                                                 |
| `0x0106` | SET_METADATA | `u32 stream`, `u32 usage` (Android `audio_usage_t`), label (empty: derived from usage)                                                             | (none): sets the PipeWire `media.name` and `media.role`                                                                                |

Limits per connection: 8 streams and 8 MiB of rings. Limits overall: 32 streams
and 32 connections. Over a limit, OPEN_OUTPUT gets `-EBUSY`; over the connection
limit, the connection is closed. Unknown stream ids get `-ENOENT`.

**The ring** is a memfd created and sealed by the bridge, mapped shared by both
sides:

```
  0  u32 magic 0x5241584c ("LXAR")   4 u32 version (1)
  8  u32 frame_bytes                12 u32 capacity_frames
 16  u32 rate                       20 u32 data_offset (4096)
 64  u64 write_pos      client: frames written (monotonic)
128  u64 read_pos       bridge: frames consumed (monotonic)
192  u64 timing_seq     bridge: seqlock, odd while the next three change
200  u64 played_frames  frames presented to the listener at ts_ns
208  u64 ts_ns          CLOCK_MONOTONIC (shared by host and session)
216  u64 latency_ns     downstream latency: PipeWire queue plus device
224  u32 state          0 idle, 1 playing, 2 dead (reopen the stream)
4096 PCM, interleaved, capacity_frames * frame_bytes bytes
```

- **Client writes:** the client copies frames to
  `data_offset + (write_pos % capacity) * frame_bytes`, publishes `write_pos`
  (release), then writes 1 to the data eventfd. It never gets more than
  `capacity` ahead of `read_pos`. When the ring is full, it waits for the space
  eventfd (POLLIN, then read it to clear).
- **Bridge reads:** the bridge takes frames when PipeWire asks for them,
  advances `read_pos` and signals the space eventfd. It ignores a `write_pos`
  that is behind `read_pos`, and clamps one more than `capacity` ahead.
- **Presentation position** at time `t` is `played_frames + (t - ts_ns) * rate`
  while `state == 1`, capped at `read_pos`. Output latency, as an app sees it,
  is `written - presented`.
- **When the bridge drops a stream** (PipeWire went away, or CLOSE), it sets
  `state = 2` and signals space, so a blocked writer wakes up.
- The client never sees the bridge's view of `read_pos` except through this
  header, which the bridge rewrites and never reads back.
