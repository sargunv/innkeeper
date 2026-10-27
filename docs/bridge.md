# The desktop bridge (`linuxapk-bridge`)

A host-side Rust daemon (`bridge/`) that gives the Android session access to the
desktop: audio, display (windows and input), and desktop integration (settings,
notifications, the clipboard, links and MPRIS). It runs on the host as the user
(glibc, outside the namespaces). Android-side clients reach it over one Unix
socket.

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
  (1041), which are mapped onto the subordinate range. The Linux.apk System app
  is admitted to the system service only: its uid is assigned at install time,
  so the bridge takes the owner of its data directory
  (`--system-app-dir <data>/data/dev.linuxapk.system`, created by installd) at
  connect time. The app reaches the directory through the `audio` group, which
  its `dev.linuxapk.permission.HOST_BRIDGE` permission maps to.
- **Log:** `out/run[-<instance>]/bridge.log`. Set `LINUXAPK_BRIDGE_VERBOSE=1`
  for per-stream statistics every 5 s.
- **Environment:**
  - `LINUXAPK_AUDIO_SINK=<sink name>` routes the session's audio to one PipeWire
    sink (the tests use a null sink). By default it goes to the desktop's
    default output.
  - The system service uses the launching shell's `DBUS_SESSION_BUS_ADDRESS`
    (the launcher passes it on) and `$WAYLAND_DISPLAY` (the clipboard).
  - Test overrides for the desktop settings, passed on by the launcher:
    `LINUXAPK_TZ`, `LINUXAPK_LOCALE` (BCP 47 list), `LINUXAPK_CLOCK`
    (`12h`/`24h`), `LINUXAPK_TEXT_SCALE`.
  - Standalone:
    `linuxapk-bridge --socket PATH [--allow-uid UID]... [--system-app-dir DIR] [--sink NAME] [--null] [-v]`.
- **Tests:** `cargo test -p linuxapk-bridge` covers the protocol, the ring, a
  live server with hostile clients, and proptest fuzzing of every decoder.
  `cargo run -p linuxapk-bridge --example tone -- SOCKET` plays a tone the way
  the Android client does.
  `--example system -- SOCKET listen|notify|clip|open|media` drives the system
  service the way the system app does.

## Code layout and conventions for new services

| Module      | What it is                                                                                                                                        |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `wire`      | Framing plus the untrusted-input helpers: `Reader`, `Writer`, `decode_header`, `reply`, `errno`                                                   |
| `transport` | The `SOCK_SEQPACKET` socket: `listen`, `accept` (with `SO_PEERCRED`), `recv`/`send` with fds                                                      |
| `proto`     | Protocol versions, `HELLO`, service ids                                                                                                           |
| `server`    | Accept loop, HELLO, the `Service` trait, `Registry`, client and uid caps                                                                          |
| `audio`     | The audio service: `proto` (its messages), `ring`, `stream` (threads), `pulse` (PipeWire output)                                                  |
| `display`   | The display service: `proto` (its messages), `wl` (the Wayland side: windows, decorations, seat)                                                  |
| `services`  | Desktop integration: `system` (the system service and its `proto`), `blob` (sealed memfds), `settings`, `notify`, `clipboard`, `openuri`, `mpris` |

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
  `FRAME`; system: the blob requests). Any other request that arrives with fds
  gets them closed and a `-EPROTO` reply.
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
- Service ids: 1 audio, 2 display, 3 system. A uid admitted to one service only
  (see "Allow-list") that asks for another gets `-EACCES` and is closed.

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

### System service (id 3)

The channel to the Linux.apk System app (`dev.linuxapk.system`,
`android/app/src/main/kotlin/dev/linuxapk/system/integration/`). It carries
desktop settings into Android; notifications, media sessions and web links out;
and the clipboard both ways. Bridge side: `bridge/src/services/`. The app opens
one connection (`BridgeClient`, reconnecting with backoff), sends `START`, and
gets events from then on. Each feature starts the first time a client asks for
it. When the connection ends, its notifications are closed and its MPRIS players
removed; on SIGTERM (session stop) the bridge closes every desktop notification
before it exits.

**Blobs.** Anything that can exceed a 512-byte message (notifications, clipboard
contents, URLs, artwork) goes as a sealed memfd next to a small request or event
that carries its length:

- from the client, the fd must be a memfd sealed with `F_SEAL_WRITE`,
  `F_SEAL_SHRINK` and `F_SEAL_GROW` (anything else, including pipes and FUSE
  files that could block, fails `F_GET_SEALS` and gets `-EINVAL`). The bridge
  reads the declared length once with `pread` into its own buffer, never maps
  it, and caps the length per message type;
- to the client, the bridge seals its memfds the same way, plus `F_SEAL_SEAL`;
- inside a blob, the layout is little-endian like messages: text is `u32 len` +
  UTF-8 (NUL refused, other control characters but newline and tab dropped),
  images are `u16 width, u16 height` + RGBA8 rows (not premultiplied; 0x0 for
  none). Images are raw on purpose: nothing Android sends reaches a host image
  decoder. The bridge builds the `image-data` hint from the pixels and encodes
  MPRIS artwork and clipboard images as PNG itself.

| Type     | Request      | Body (fd)                                                                       | Reply                                                                                          |
| -------- | ------------ | ------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `0x0301` | START        | `u32 features` (1 settings, 2 clipboard, 4 notifications, 8 media, 16 links)    | `u32` features served; the current settings and host clipboard follow as events                |
| `0x0302` | NOTIFY       | `u32 id` (≠ 0), `u32 len` ≤ 1 MiB (fd: notification blob)                       | `-EBUSY` over 64 per connection, `-EIO` if the server refused; posting an id again replaces it |
| `0x0303` | NOTIFY_CLOSE | `u32 id`                                                                        |                                                                                                |
| `0x0304` | CLIP_SET     | `u8 kind` (1 text, 2 image), `u8 sensitive`, `u16 0`, `u32 len` ≤ 1/16 MiB (fd) | `-ENOSYS` without data control                                                                 |
| `0x0305` | OPEN_URI     | `u32 len` ≤ 8192 (fd: the URL)                                                  | `-EINVAL` if not http(s), `-EAGAIN` over 5 per 10 s                                            |
| `0x0306` | MEDIA        | `u32 player` (≠ 0), `u32 len` ≤ 2 MiB (fd: media blob)                          | `-EBUSY` over 8 players per connection                                                         |
| `0x0307` | MEDIA_CLOSE  | `u32 player`                                                                    |                                                                                                |

| Event    | Message      | Body (fd)                                                                                                                                                                                                |
| -------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `0x4301` | SETTINGS     | `u8 color_scheme` (0 none, 1 dark, 2 light), `u8 has_accent`, `u8 r, g, b`, `u8 contrast`, `u8 reduced_motion`, `u8 clock` (0, 1 12h, 2 24h), `f32 text_scale` (0 unknown), name time zone, name locales |
| `0x4302` | NOTIFY_EVENT | `u32 id`, `u8 what` (1 clicked, 2 action, 3 dismissed, 4 reply), `u8 action`, `u16 0`, `u32 len` (fd: the reply text, for 4)                                                                             |
| `0x4303` | CLIP         | `u8 kind`, `u8 sensitive`, `u16 0`, `u32 len` (fd: the content)                                                                                                                                          |
| `0x4304` | MEDIA_CMD    | `u32 player`, `u8 cmd` (1 play, 2 pause, 3 play/pause, 4 stop, 5 next, 6 previous, 7 seek by, 8 seek to, 9 raise), `u8 0`, `u16 0`, `i64 arg` (µs)                                                       |

Blob layouts (each starts with `u32 version` = 1): `services::notify::Note` and
`services::mpris::Media` document theirs, and the system app's encoders
(`Notifications.kt`, `MediaSync.kt`) mirror them. Every decoder has proptests.

#### Desktop settings

One watcher per bridge (`services/settings.rs`); each change sends a full
`SETTINGS`. The app applies a setting when the desktop's value changes (and once
after connecting, if Android differs), so a change made inside Android lasts
until the desktop changes it again. `persist.linuxapk.sync.settings=0` turns it
off.

| Desktop (source)                                                         | Android                                                                                                                                                                                                                                                                                                                                               |
| ------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Portal Settings `org.freedesktop.appearance` `color-scheme`, live        | `UiModeManager.setNightMode` (yes/no; "no preference" leaves Android alone)                                                                                                                                                                                                                                                                           |
| `accent-color`, live                                                     | Material You seed: `Settings.Secure` `theme_customization_overlay_packages` (`system_palette`/`accent_color` = the color, `color_source` = `preset`), which SystemUI's ThemeOverlayController turns into fabricated overlays. Android 17's `IThemeManager` (service `theme`) sits behind the `enable_theme_service` flag, which is off in both images |
| `contrast`, `reduced-motion`                                             | `contrast_level`; the three animation scales (0 when reduced)                                                                                                                                                                                                                                                                                         |
| `org.gnome.desktop.interface` `clock-format` (KDE's portal fills it too) | `Settings.System.TIME_12_24` plus the `TIME_CHANGED` broadcast Settings sends (allowed for persistent apps)                                                                                                                                                                                                                                           |
| `org.gnome.desktop.interface` `text-scaling-factor` (KDE fills it too)   | `Settings.System.FONT_SCALE`, clamped to 0.85–2                                                                                                                                                                                                                                                                                                       |
| `/etc/localtime` (or `TZ`), polled every 5 s                             | `AlarmManager.setTimeZone`, after turning off automatic time-zone detection                                                                                                                                                                                                                                                                           |
| `LANGUAGE`, then `LC_ALL`/`LC_MESSAGES`/`LANG` of the bridge, at start   | `LocalePicker.updateLocales` (`de_DE.UTF-8@euro` → `de-DE`; `@latin` → `-Latn-`)                                                                                                                                                                                                                                                                      |

#### Notifications

`services/notify.rs` talks to `org.freedesktop.Notifications` directly: unlike
the portal, it can replace a notification in place, show progress, and offer
inline replies. (The portal is the route inside Flatpak, later.) The app's
`NotificationListenerService` (`NotificationForwarder`, access granted by the
app itself) forwards posts, updates and removals; it skips group summaries,
media notifications (MPRIS covers them) and the system's own (`android`,
SystemUI). Updates are throttled to 2/s and identical reposts dropped (timers
repost every second; KDE refuses "similar notifications in quick succession").

- Title, text (big text, inbox lines, or the last 5 messages of a
  MessagingStyle), app name, and the large icon or picture as `image-data` (the
  app icon instead when the desktop has no `.desktop` file for it). The body is
  markup-escaped when the server parses markup.
- Hints: `desktop-entry` = `dev.linuxapk.<package>` (the CLI's desktop files),
  `urgency` from the channel importance, `value` for progress, `resident` and no
  timeout for ongoing ones, `suppress-sound` for silent updates.
- Actions: `default` (when there is a content intent) and up to 3 buttons. The
  first action with a free-form `RemoteInput` becomes KDE's `inline-reply`
  (placeholder from its label); `NotificationReplied` goes back through
  `RemoteInput.addResultsToIntent`.
- A click sends the content intent. If the app has a window, the activity is
  launched on that display and the window is raised through the window manager;
  otherwise the window manager adopts it from display 0. Auto-cancel
  notifications are then cancelled.
- Dismissal syncs both ways: Android cancelling closes the desktop notification;
  `NotificationClosed` reason 2 (dismissed) cancels the Android one. Reason 1
  (expired) keeps both: the server may keep it in its history, where updates and
  actions still reach it. KDE reports closing a popup as "expired" too; removing
  it from the history (bell) reports "dismissed".
- Diagnostic:
  `am start -n dev.linuxapk.system/.integration.notifications.NotificationProbe`
  posts a message-style notification with an action and an inline reply.

#### Clipboard

`services/clipboard.rs` has its own Wayland connection and uses
`ext_data_control_v1`, which works without keyboard focus. Decision: this, and
not a hook in the display service's seat, because data control needs no focus
and no window, so the clipboard also works with no Android window open or
focused, and the two services stay independent. KDE (KWin 6), wlroots
compositors and others offer it. **GNOME doesn't**: there the clipboard needs a
`wl_data_device` on the display service's seat, used while one of our windows
has focus. That hook (the display service hands the clipboard service its seat
and focus events) is the follow-up; `clipboard: unavailable` in the log means
the compositor has no data control.

- Host to Android: each new selection with text (or else `image/png`) is read (1
  MiB text, 16 MiB / 2048x2048 images, 5 s timeout) and sent as `CLIP`. On
  Android it becomes the primary clip; images are served by the app's
  `ClipImageProvider` (`content://dev.linuxapk.system.clipboard/<n>.png`, not
  exported; ClipboardService grants the pasting app access).
- Android to host: the app's primary-clip listener (it holds
  `READ_CLIPBOARD_IN_BACKGROUND`) sends text, or an `image/*` URI's pixels. The
  bridge's data source offers the text types (or `image/png`) plus a private
  marker type, so the selection event that follows is recognised as its own. The
  app remembers what each side last sent, so nothing bounces.
- Sensitive clips (`ClipDescription.EXTRA_IS_SENSITIVE`) carry
  `x-kde-passwordManagerHint: secret`, which keeps them out of Klipper's
  history; host clips with that hint arrive marked sensitive.
- Diagnostic:
  `am start -n dev.linuxapk.system/.integration.clipboard.ClipProbe [--es text T | --ez image true]`
  sets the clip, or logs it.

#### Opening links

- The app's `BrowserActivity` handles `VIEW` http/https with `BROWSABLE` at
  priority −100, so verified app links and real browsers win. It shows nothing,
  converts the URL to percent-encoded ASCII and sends `OPEN_URI`.
- `CustomTabsService` answers `warmup`/`newSession` (a hand-written
  `ICustomTabsService` binder), so Custom Tabs and AppAuth see a browser; the
  tab itself is a `VIEW` to `BrowserActivity`.
- **Default browser** (`BrowserRole`): both images ship the WebView test shell
  (`org.chromium.webview_shell`), which apps would pick. The app turns its
  browser activity off and takes the browser role when nobody else holds it. A
  browser installed later takes the role over. `persist.linuxapk.browser`:
  `auto` (default), `host` (always ours), `android` (never).
- The bridge accepts only absolute `http(s)` URLs with a host, in printable
  ASCII without spaces, quotes or backslashes, at most 8192 bytes and 5 per 10 s
  per connection; it logs each one (the first 512 bytes) and calls the OpenURI
  portal, or `xdg-open URL` without a shell if there is no portal.
- OAuth: the authorize URL opens in the host browser; the redirect to the app's
  custom scheme comes back through the CLI's scheme handler (`linuxapk run`),
  which the window manager delivers to the app's window. https App Link
  redirects still need the sign-in window (see
  [integration.md](integration.md)).

#### Media (MPRIS)

The app watches `MediaSessionManager.getActiveSessions` (through its enabled
listener) and sends `MEDIA` when a session's metadata, state, actions or artwork
change, or its position jumps. Position-only progress isn't sent: the bridge
extrapolates while playing. Each player is a session-bus connection owning
`org.mpris.MediaPlayer2.dev_linuxapk_<package>` (`.instance…` for a second one)
with `/org/mpris/MediaPlayer2`:

- `Identity` (app label), `DesktopEntry` (`dev.linuxapk.<package>`),
  `PlaybackStatus` (buffering counts as playing), `Metadata` (`mpris:trackid`,
  `mpris:length`, `xesam:title`, `xesam:artist`, `xesam:album`, `mpris:artUrl`),
  `Position`, `Rate`, `Can*` from the session's actions, and `Seeked` when the
  position jumps;
- calls become `MEDIA_CMD` and then transport controls; `Raise` raises the app's
  window. Artwork is written as PNG to `out/run[-<instance>]/art/` (not the
  shared socket directory) and removed with the player.

#### Changes outside `services/` (for the other owners)

- `server`: `Config::service_uids` (`UidCheck`), uids admitted to one service
  only; HELLO for another service gets `-EACCES`. `wire`: `EACCES`, and
  `DecodeError` implements `Error`. `main.rs`: registration, `--system-app-dir`,
  the SIGTERM handler.
- `session/linuxapk-session`: `--system-app-dir`, and passes
  `DBUS_SESSION_BUS_ADDRESS` and the test overrides to the session unit.
- System app: `WindowManagerFeature` doesn't adopt the app's own tasks from
  display 0 (`BrowserActivity` finishes at once and needs no window).

#### Testing on a desktop you can't disturb

A nested Plasma with its own config, data and bus, so nothing touches the real
desktop:

```sh
N=out/nested   # config/ data/ cache/ state/ home/ and hold.sh (writes $N/bus)
systemd-run --user --unit linuxapk-integ-kwin -p MemoryMax=3G \
  --setenv=HOME=$N/home --setenv=XDG_CONFIG_HOME=$N/config --setenv=XDG_DATA_HOME=$N/data \
  --setenv=XDG_CACHE_HOME=$N/cache --setenv=XDG_STATE_HOME=$N/state --setenv=XDG_CURRENT_DESKTOP=KDE \
  -- dbus-run-session -- kwin_wayland --virtual --no-lockscreen --socket lapk-integ \
  --width 1920 --height 1080 --exit-with-session $N/hold.sh
# then plasmashell in the same environment (its own unit), and:
WAYLAND_DISPLAY=lapk-integ DBUS_SESSION_BUS_ADDRESS=$(cat $N/bus) session/linuxapk-session start
```

- Set `HOME` too: the nested kded's GTK module writes `$HOME/.gtkrc-2.0`. Change
  the nested color scheme only with every one of those variables set
  (`plasma-apply-colorscheme BreezeDark`, `--accent-color '#e95420'`).
- A fake default browser (a `.desktop` for `x-scheme-handler/https` in
  `$N/data/applications`, `mimeapps.list` in `$N/config`) logs what OpenURI
  opens instead of starting a real browser.
- A null sink (`LINUXAPK_AUDIO_SINK`) keeps media tests silent.
