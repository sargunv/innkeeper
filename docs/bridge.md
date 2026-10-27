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
| `display`   | The display service: `proto` (its messages), `wl` (the Wayland side: windows, decorations, seat) |

To add a service (the windowing lane's display service is id 2, types
`0x0200..=0x02ff`):

1. **Id and types.** Take a service id below 64 in `proto::service`. Its
   requests use `0xNN01..=0xNNff` (NN = the id). Requests live below `0x4000`,
   and the core answers anything at or above it with `-EPROTO`. Events (bridge
   to client, tag 0) use `0x4000..=0x7fff`; take `0x40NN`-style sub-ranges per
   service and list them here.
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
   Dropping the service is the connection's cleanup. Put per-client caps
   (memory, objects) in the service, as `audio::Limits` does.
4. **Events.** The factory receives a `server::Events` for the connection.
   `Events::send(ty, body, fds)` pushes one event packet (tag 0; the type must
   be in the event range; at most 512 bytes and 4 fds). It is cheap to clone, so
   hand it to your threads. Each event is one packet, so events never split a
   reply, but their order relative to replies isn't defined. Clients match
   replies by tag and treat tag-0 event types separately. Sends fail once the
   client is gone.
5. **Registration.** Register a factory with
   `Registry::register(id, name, |peer, events| ...)` in `main.rs`. If the
   service has new clients, extend the launcher's `--allow-uid` list and
   document them below.
6. **Shared memory and fds you hand out.** Create them in the bridge, and seal
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
  (`SCM_RIGHTS`, at most 4). Services may also send events (types
  `0x4000..=0x7fff`, tag 0) at any time (`server::Events`, passed to the
  factory); clients that don't expect any can drop them. `Events::try_send`
  never blocks (a client that doesn't read its socket loses those events), and
  `Events::shutdown` ends the connection, so the client sees EOF and can
  reconnect.
- Requests carry fds only where their service says so (display: `BUFFER_ADD`,
  `FRAME`). Any other request that arrives with fds gets them closed and a
  `-EPROTO` reply.
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

### Display service (id 2)

Host windows for the composer (the Android HWC HAL), which is the only client:
it connects as `system` from inside the session. The composer keeps the HWC and
virtual-display side and the window registry ([windowing.md](windowing.md)); the
bridge owns the Wayland connection (one per client connection, on its own thread
with a calloop loop; `bridge/src/display/wl.rs`), the toplevels, their
decorations and the seat. Windows are named by the Android display id they show;
buffers by ids the composer picks. The bridge needs `$WAYLAND_DISPLAY`, which
the session launcher sets to the host socket.

| Type     | Message             | Body                                                                                                                                                                               | Notes                                                                                                                                         |
| -------- | ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `0x0201` | WINDOW_CREATE       | `u32 window`, `u32 width`, `u32 height` (logical, 1–16384), `u32 min_width`, `u32 min_height` (0–16384, 0: 240x160), name `app_id`, name `title`                                   | Replies once the toplevel exists; `-EBUSY` for a duplicate id or over the limit                                                               |
| `0x0202` | WINDOW_DESTROY      | `u32 window`                                                                                                                                                                       | Drops the window's buffers too                                                                                                                |
| `0x0203` | WINDOW_SET_TITLE    | `u32 window`, name                                                                                                                                                                 |                                                                                                                                               |
| `0x0204` | WINDOW_SET_MIN_SIZE | `u32 window`, `u32 width`, `u32 height` (logical)                                                                                                                                  |                                                                                                                                               |
| `0x0205` | WINDOW_ACTIVATE     | `u32 window`                                                                                                                                                                       | `xdg_activation_v1` with the newest input serial                                                                                              |
| `0x0206` | BUFFER_ADD          | `u32 buffer`, `u32 window`, `u32 width`, `u32 height`, `u32 fourcc` (XBGR/ABGR/XRGB/ARGB8888), `u32 stride` (≥ 4×width), `u32 offset`, `u64 modifier` (0, LINEAR); fd: the dma-buf | The fd's size (`lseek`) must cover `offset + stride × height`; the compositor imports it through linux-dmabuf                                 |
| `0x0207` | BUFFER_REMOVE       | `u32 buffer`                                                                                                                                                                       |                                                                                                                                               |
| `0x0208` | FRAME               | `u32 window`, `u32 buffer`, `i32 x, y, w, h` damage (zero: everything); optional fd: acquire fence (ignored)                                                                       | The buffer is the composer's until RELEASE; a frame for a window that isn't configured yet, or a buffer not imported yet, is released at once |
| `0x0209` | TEXT_INPUT          | `u16 len` + one `ti_*` line from the Linux.apk IME (≤ 480 bytes), relayed by the composer                                                                                          | Applied to the window with text-input focus; see "Text input" below                                                                           |

| Event    | Message   | Body                                                                                   |
| -------- | --------- | -------------------------------------------------------------------------------------- |
| `0x4201` | CONFIGURE | `u32 window`, `u32 width`, `u32 height` (logical), `u32 scale×120`                     |
| `0x4202` | CLOSE     | `u32 window` (the user closed it: title bar button or compositor)                      |
| `0x4203` | RELEASE   | `u32 buffer` (mirrors `wl_buffer.release`)                                             |
| `0x4204` | INPUT     | `u16 len` + one input protocol v2 line (≤ 400 bytes, [hal-display.md](hal-display.md)) |

- **Decorations:** server-side through xdg-decoration where the compositor
  offers it (KDE); else client-side, an Adwaita-style frame from `sctk-adwaita`
  with title, minimize, maximize, close, move, resize and the window menu
  (GNOME, weston). `LINUXAPK_BRIDGE_CSD=1` forces the client-side frame for
  testing.
- **Frames:** each buffer is shown through `wp_viewporter` at the window's
  logical size (fractional scale through `wp_fractional_scale_v1`); input
  coordinates map from surface units to the shown buffer's pixels.
- **Input:** the bridge turns `wl_pointer`, `wl_keyboard` and `wl_touch` into
  protocol v2 lines with the window id on the enter lines; pointer events on the
  client-side frame drive the frame instead. The composer relays the lines on
  `/dev/socket/linuxapk-input`, so the Linux.apk System app is unchanged.
- **Limits** per connection: 32 windows, 128 buffers. Unknown windows or buffers
  get `-ENOENT`.
- **Compositor gone:** the service shuts the client connection; the composer
  reconnects every second and asks for its windows again (its buffers are
  re-added with the next frame).
- **Test:** `cargo run -p linuxapk-bridge --example window -- SOCKET` opens a
  window with a udmabuf pool the way the composer does and prints the events.
- **Scale:** `wp_fractional_scale_v1` where the compositor has it; otherwise the
  integer `wl_surface.preferred_buffer_scale` (or the outputs' scale), with
  `wl_surface.set_buffer_scale` when there is no viewporter. Either way a change
  sends CONFIGURE with the new `scale×120`.

#### Text input

The display service drives `zwp_text_input_v3` for the Linux.apk IME
(`display/ime.rs` is the line protocol, `display/wl/text_input.rs` the Wayland
side; [input.md](input.md#text-input) has the Android side). There is one text
input per seat, and it follows the keyboard focus: the compositor sends
`enter`/`leave` for our windows' surfaces.

- **IME to bridge** (TEXT_INPUT, through the composer), applied only while a
  window has text-input focus and in order:

  | Line                                             | Effect                                                                                                                                |
  | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------- |
  | `ti_enable` / `ti_disable`                       | `enable` / `disable`                                                                                                                  |
  | `ti_content <hint> <purpose>`                    | `set_content_type` (hint ≤ `0x3ff`, purpose ≤ 13)                                                                                     |
  | `ti_surrounding <cursor> <anchor> <0\|1> <text>` | `set_surrounding_text`; byte offsets on char boundaries; 1 also sets the change cause to `other` (the user or the app moved the text) |
  | `ti_cursor <x> <y> <w> <h>`                      | `set_cursor_rectangle`, from the display's pixels divided by the window's scale                                                       |
  | `ti_apply`                                       | `commit`                                                                                                                              |

- **Bridge to IME**, as ordinary INPUT lines at each `done`, so they stay in
  order with the keys: `ti_delete <before> <after>` (bytes), `ti_commit <text>`
  (repeated for long text; the IME appends), `ti_preedit <begin> <end> [<text>]`
  (always sent; no text means no preedit), then `ti_done <window>`. On `leave`
  the IME gets an empty preedit and `ti_done`, as the protocol asks.
- **Text encoding:** bytes up to space, DEL and `%` are `%XX`; everything else
  (UTF-8) goes as is. The parser accepts only that canonical form (each line
  must re-encode to itself), bounded lengths and numbers, and no NUL.
- Without a host input method (IBus, Fcitx) enabling changes nothing: keys keep
  arriving as `wl_keyboard` keys.
