# The desktop bridge (`linuxapk-bridge`)

A host-side Rust daemon (`bridge/`) that gives the Android session access to the
desktop: audio, display (windows and input), and desktop integration (settings,
notifications, the clipboard, links and MPRIS). It runs on the host as the user
(glibc, outside the namespaces). Android-side clients reach it over one Unix
socket.

The bridge is a **security boundary**. Everything it reads from a client is
untrusted: message bodies, shared memory, and fds the client may still hold.
Nothing a client does may block the bridge: events never wait for a client
(conventions 4 and 6), and limits on work that outlives a connection, such as
document picks, are bridge-wide (from a security review, 2026-10-27).

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
  is admitted to the system, documents and apps services only: its uid is
  assigned at install time, so the bridge takes the owner of its data directory
  (`--system-app-dir <data>/data/dev.linuxapk.system`, created by installd) at
  connect time. The input service is the system app's alone: no allow-listed uid
  may open it. The app reaches the directory through the `audio` group, which
  its `dev.linuxapk.permission.HOST_BRIDGE` permission maps to.
- **Log:** `out/run[-<instance>]/bridge.log`. Set `LINUXAPK_BRIDGE_VERBOSE=1`
  for per-stream statistics every 5 s.
- **Environment:**
  - `LINUXAPK_AUDIO_SINK=<sink name>` routes the session's audio to one PipeWire
    sink (the tests use a null sink). By default it goes to the desktop's
    default output.
  - The system service uses the launching shell's `DBUS_SESSION_BUS_ADDRESS`
    (the launcher passes it on) and `$WAYLAND_DISPLAY` (the clipboard).
  - Standalone:
    `linuxapk-bridge --socket PATH [--allow-uid UID]... [--system-app-dir DIR] [--mirror-apps] [--grants FILE] [--sink NAME] [-v]`.
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
   `Events::try_send(ty, body, fds)` pushes one event packet (tag 0; the type
   must be in the event range; at most 512 bytes and 4 fds). It is cheap to
   clone, so hand it to your threads. Each event is one packet, so events never
   split a reply, but their order relative to replies isn't defined. Clients
   match replies by tag and treat tag-0 event types separately. Sends fail once
   the client is gone, and never wait: a client whose socket is full loses the
   event (EAGAIN). There is no blocking variant, because a slow client could
   otherwise stall a service, and everyone behind its locks.
5. **Registration.** Register a factory with
   `Registry::register(id, name, |peer, events| ...)` in `main.rs`. If the
   service has new clients, extend the launcher's `--allow-uid` list and
   document them below.
6. **Shared memory and fds you hand out.** Create them in the bridge, and seal
   memfds against shrinking (`F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`) so a
   client can't truncate them under your mapping and SIGBUS the bridge. A shared
   eventfd shares its file flags: the client can clear `O_NONBLOCK`, drain it or
   saturate it. Read one with `preadv2(RWF_NOWAIT)`, and signal the client
   through a pipe whose write end only the bridge holds (writes can't be made
   non-blocking per call). Never read back state you own from shared memory;
   keep it in the bridge and mirror it out. Clamp whatever the client writes
   there.

## Protocol v1

Transport: `AF_UNIX`, `SOCK_SEQPACKET`. One packet is one message. All integers
are little-endian.

Android-side C and C++ clients (the composer, `libtinyalsav2`) share its client
half: `android/vendor/bridge-client`, a static library that connects, says
HELLO, frames messages, passes fds and matches replies by tag.

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
- Service ids: 1 audio, 2 display, 3 system, 4 documents, 5 apps, 6 input. A uid
  admitted to some services only (see "Allow-list") that asks for another gets
  `-EACCES` and is closed.

### Audio service (id 1)

| Type     | Message      | Body                                                                                                                                               | Reply                                                                                                                                          |
| -------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `0x0101` | OPEN_OUTPUT  | `u32 rate` (8000–192000), `u8 channels` (1–2), `u8 format` (1 s16le, 2 f32le), `u16 0`, `u32 period_frames` (16–16384), `u32 periods` (2–16), name | `u32 stream`, `u32 capacity_frames`, `u32 data_offset`, `u32 map_size`, `u32 latency_us`; fds: ring memfd, data eventfd, space pipe (read end) |
| `0x0102` | CLOSE        | `u32 stream`                                                                                                                                       | (none)                                                                                                                                         |
| `0x0103` | STANDBY      | `u32 stream`                                                                                                                                       | (none): once the ring drains and PipeWire underruns, the stream is corked so the sink can suspend                                              |
| `0x0104` | SET_VOLUME   | `u32 stream`, `f32 left`, `f32 right` (each 0–1)                                                                                                   | (none)                                                                                                                                         |
| `0x0105` | SET_MASTER   | `f32 volume` (0–1), `u8 muted` (0/1); applies to all of the connection's streams, current and future                                               | (none)                                                                                                                                         |
| `0x0106` | SET_METADATA | `u32 stream`, `u32 usage` (Android `audio_usage_t`), label (empty: derived from usage)                                                             | (none): sets the PipeWire `media.name` and `media.role`                                                                                        |

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
  pipe (POLLIN, then read up to 8 bytes to clear it).
- **Bridge reads:** the bridge takes frames when PipeWire asks for them,
  advances `read_pos` and signals the space pipe (one byte, unless one is
  already pending). It ignores a `write_pos` that is behind `read_pos`, and
  clamps one more than `capacity` ahead.
- **Presentation position** at time `t` is `played_frames + (t - ts_ns) * rate`
  while `state == 1`, capped at `read_pos`. Output latency, as an app sees it,
  is `written - presented`.
- **When the bridge drops a stream** (PipeWire went away, or CLOSE), it sets
  `state = 2` and signals space, so a blocked writer wakes up.
- The client never sees the bridge's view of `read_pos` except through this
  header, which the bridge rewrites and never reads back.

### Display service (id 2)

The composer's HWC virtual displays in host windows. The composer (the Android
HWC HAL) is the only client; it connects as root from inside the session and
only presents: it adds each virtual display (`DISPLAY_ADD`) and sends its frames
as dma-bufs of its own udmabuf pool. The bridge owns the Wayland connection (one
per composer connection, on its own thread with a calloop loop;
`host/bridge/src/display/wl.rs`), the toplevels, their decorations and the seat.
A display gets a window only if the window manager asked for one of its size on
the input service; the rest stay headless. The wire spec is the module doc of
`host/bridge/src/display/proto.rs`. The bridge needs `$WAYLAND_DISPLAY`, which
the session launcher sets to the host socket.

- **Decorations:** server-side through xdg-decoration where the compositor
  offers it (KDE); else client-side, an Adwaita-style frame from `sctk-adwaita`
  with title, minimize, maximize, close, move, resize and the window menu
  (GNOME, weston).
- **Frames:** each buffer is shown through `wp_viewporter` at the window's
  logical size (fractional scale through `wp_fractional_scale_v1`); input
  coordinates map from surface units to the shown buffer's pixels.
- **Limits** per connection: 32 displays, 128 buffers. Unknown displays or
  buffers get `-ENOENT`.
- **Compositor gone:** the service shuts the client connection; the composer
  reconnects every second and adds its displays again (its buffers are re-added
  with the next frame).
- **Test:** `cargo run -p linuxapk-bridge --example window -- SOCKET` asks for a
  window on the input service the way the window manager does, then adds a
  display with a udmabuf pool the way the composer does, and prints the input
  service's events. The input service needs the bridge's `--system-app-dir` to
  be a directory you own.
- **Scale:** `wp_fractional_scale_v1` where the compositor has it; otherwise the
  integer `wl_surface.preferred_buffer_scale` (or the outputs' scale), with
  `wl_surface.set_buffer_scale` when there is no viewporter. Either way a change
  sends a `window` line with the new scale.

### Input service (id 6)

The Linux.apk System app's side of the windows: its window manager asks for
windows (`WINDOW_REQUEST`, matched to the composer's next display of that size),
its injector gets the host seat as protocol v2 input lines, and its IME drives
the host's input methods. Only the system app's uid may open it, even where the
allow-list admits a uid to every other service (`Config::private_services`): the
lines are the user's keystrokes. The wire spec, including every input line, is
the module doc of `host/bridge/src/input/proto.rs`; `input/hub.rs` is what it
shares with the display service.

- **Input:** the seat (`input/seat.rs`) turns `wl_pointer`, `wl_keyboard` and
  `wl_touch` into lines with the window's Android display id on the enter lines;
  pointer events on a client-side frame drive the frame instead. Window size
  changes and close requests are lines too, so they stay in order with the
  input.
- **Windows:** a request waits up to 10 s for its display. `WINDOW_MAPPED` and
  `WINDOW_UNMAPPED` tell the window manager when a display shows in its window
  and when that ends (the display went away, or the composer did); it then asks
  again.

#### Text input

The seat drives `zwp_text_input_v3` for the Linux.apk IME (`input/ime.rs` is the
line protocol, with its table of lines, and `input/text_input.rs` the Wayland
side; [input.md](input.md#text-input) has the Android side). There is one text
input per seat, and it follows the keyboard focus: the compositor sends
`enter`/`leave` for our windows' surfaces.

- **IME to bridge:** `TEXT_INPUT` requests, applied only while a window has
  text-input focus and in order.
- **Bridge to IME:** ordinary input lines at each `done`, so they stay in order
  with the keys.
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
one connection (`BridgeConnection`, reconnecting with backoff), sends `START`,
and gets events from then on. Each feature starts the first time a client asks
for it. When the connection ends, its notifications are closed and its MPRIS
players removed; on SIGTERM (session stop) the bridge closes every desktop
notification before it exits.

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
until the desktop changes it again.

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
  browser installed later takes the role over.
- The bridge accepts only absolute `http(s)` URLs with a host, in printable
  ASCII without spaces, quotes or backslashes, at most 8192 bytes and 5 per 10 s
  per connection; it logs each one (the first 512 bytes) and calls the OpenURI
  portal, or `xdg-open URL` without a shell if there is no portal.
- **Test sessions** (a named `LINUXAPK_INSTANCE`, or `LINUXAPK_TEST=1`, which
  `mise run test:compat` sets) never touch the user's real desktop. The launcher
  passes `--no-open-urls`, so links are validated and logged
  (`openuri: …: not opened (test session): URL`) but not opened, unless
  `LINUXAPK_OPEN_URLS=1`. It also passes `--no-desktop-presence` unless the
  session talks to a private bus (a nested test desktop): notifications and
  MPRIS players are then only logged. The compat step `bridgelog:<text>` checks
  the bridge log for such a line.
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

A nested Plasma, fully isolated as AGENTS.md requires ("Nested test desktops are
fully isolated"): a private `dbus-run-session`, `XDG_RUNTIME_DIR`, `HOME` and
XDG dirs under `out/`, `DISPLAY` unset, and `WAYLAND_DISPLAY` set to the nested
socket _before_ the bus starts, so the portals, kded and anything else the
private bus activates inherit it rather than the host's `wayland-0`:

```sh
N=$PWD/out/nested   # rt/ home/ config/ data/ cache/ state/, hold.sh writes $N/bus
E=(-u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS HOME=$N/home XDG_RUNTIME_DIR=$N/rt
   XDG_CONFIG_HOME=$N/config XDG_DATA_HOME=$N/data XDG_CACHE_HOME=$N/cache XDG_STATE_HOME=$N/state
   XDG_CURRENT_DESKTOP=KDE)
systemd-run --user --unit linuxapk-integ-kwin -p MemoryMax=3G -- env "${E[@]}" WAYLAND_DISPLAY=lapk-integ \
  dbus-run-session -- kwin_wayland --virtual --no-lockscreen --socket lapk-integ \
  --width 1920 --height 1080 --exit-with-session $N/hold.sh
# plasmashell, if needed, in its own unit with the same env plus the nested bus
WAYLAND_DISPLAY=$N/rt/lapk-integ DBUS_SESSION_BUS_ADDRESS=$(cat $N/bus) session/linuxapk-session start
```

- Check the environment of every process on the nested bus
  (`/proc/<pid>/environ`: private `HOME`, nested `WAYLAND_DISPLAY`) before
  testing, and stop both units when done.
- Change the nested color scheme only from a shell with all of the above set
  (`plasma-apply-colorscheme BreezeDark`, `--accent-color '#e95420'`), and guard
  against the real `HOME`: the nested kded writes `$HOME/.gtkrc-2.0`.
- A fake default browser (a `.desktop` for `x-scheme-handler/https` in
  `$N/data/applications`, `mimeapps.list` in `$N/config`) logs what OpenURI
  opens instead of starting a real browser.
- A null sink (`LINUXAPK_AUDIO_SINK`) keeps media tests silent.

### Documents service (id 4)

Host files and folders the user picks in the desktop's file chooser, for the
Linux.apk System app's document picker (`documents/PickerActivity.kt`) and its
DocumentsProvider (`HostDocumentsProvider.kt`, authority
`dev.linuxapk.system.documents`). Code: `bridge/src/services/documents/`. Only
the system app's uid may open it (`--system-app-dir`).

The bridge is the only side that sees host paths. Android gets opaque **grant**
ids (u64) and file descriptors:

- A grant is made only from a FileChooser portal result, so Android can reach
  only what the user picked. Folder grants cover everything beneath the folder.
- Grants persist in `--grants FILE` (the session passes
  `out/run[-<instance>]/documents.grants`), so persisted URI permissions keep
  working across restarts. At most 4096 are kept; FORGET drops one.
- Paths are relative to a folder grant: `/`-separated names, none empty, `.` or
  `..`, at most 400 bytes. They are opened with
  `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` from the folder's fd, so
  `..`, absolute or escaping symlinks and `/proc` magic links can't leave it.
- Only regular files are opened: the type is checked through an `O_PATH` fd
  first (a FIFO would block), the real open is non-blocking and must be the same
  inode. Writes, creates, deletes and renames need a grant made for writing;
  deletes and renames only apply beneath a folder grant (never to the picked
  file or folder itself), and folders are deleted only when empty.

| Type     | Message | Body                                                                                                                                                                                                   | Reply                                                                                                                                                                                                  |
| -------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `0x0401` | PICK    | `u32 request` (client's id), `u8 mode` (1 open, 2 open several, 3 save, 4 folder), `u8 write`, `u32 window` (Android display id, 0 none), name `title`, name `suggested`, `u8 n` (≤ 16) + n MIME names | (none) at once; the result comes as events. `-EBUSY` with 4 picks open in the whole bridge (a pick stays open until the user answers, even if its client disconnects), `-ENOSYS` without a session bus |
| `0x0402` | OPEN    | `u64 grant`, `u8 mode` (1 read, 2 write+truncate, 3 read-write, 4 append), path                                                                                                                        | fd: the open file                                                                                                                                                                                      |
| `0x0403` | STAT    | `u64 grant`, path                                                                                                                                                                                      | meta                                                                                                                                                                                                   |
| `0x0404` | LIST    | `u64 grant`, path (a folder)                                                                                                                                                                           | `u32 len`, `u32 count`; fd: sealed memfd with `count` metas (regular files and folders, ≤ 10000)                                                                                                       |
| `0x0405` | CREATE  | `u64 grant`, path (the folder), `u8 kind` (0 file, 1 folder), file name                                                                                                                                | `u16 len` + the name used (`name (1).ext` if taken)                                                                                                                                                    |
| `0x0406` | DELETE  | `u64 grant`, path                                                                                                                                                                                      | (none)                                                                                                                                                                                                 |
| `0x0407` | RENAME  | `u64 grant`, path, new file name (same folder, never replacing)                                                                                                                                        | (none)                                                                                                                                                                                                 |
| `0x0408` | FORGET  | `u64 grant`                                                                                                                                                                                            | (none)                                                                                                                                                                                                 |

Paths and file names are `u16 len` + UTF-8 (paths ≤ 400 bytes, names ≤ 255, no
NUL or `/` in a name). A meta is `u8 kind`, `u64 size`, `i64 mtime_ms`,
`u8 writable`, `u16 len` + name.

| Event    | Message   | Body                                                                                       |
| -------- | --------- | ------------------------------------------------------------------------------------------ |
| `0x4401` | PICKED    | `u32 request`, `u64 grant`, `u8 kind`, `u8 writable`, name (display name), one per item    |
| `0x4402` | PICK_DONE | `u32 request`, `i32 status` (0, `-ECANCELED` when the user cancelled, `-EIO`), `u32 count` |

The dialog is the FileChooser portal's `OpenFile` (with `multiple` or
`directory`) or `SaveFile` (with `current_name`), modal, with a filter for the
app's MIME types plus "All files". Its parent is the app's window, exported
through `zxdg_exporter_v2` by the display service (`wayland:<handle>`). A saved
file is created empty at once, as Android's `CREATE_DOCUMENT` expects.

Proof (2026-10-27, nested KWin 6.7 with the KDE portal, CI image): in Editor
(`org.billthefarmer.editor`, installed from F-Droid), "Storage" in its open
dialog sent `OPEN_DOCUMENT`; KDE's "Open File" dialog came up centered on the
Editor window, modal over it (parent `wayland:<handle>`). Picking a host
`hello.txt` returned
`content://dev.linuxapk.system.documents/document/52a263e898716b97`, Editor
showed the file, and Ctrl+S wrote the edit back to the host file. Unknown grant
ids and `..` paths are refused (`cargo test -p linuxapk-bridge documents` covers
symlinks out of a folder, FIFOs and writes without a write grant).

- **Testing:** in a fully isolated nested desktop ("Testing on a desktop you
  can't disturb" above). The portal must be activated on the private bus with
  the nested `WAYLAND_DISPLAY`, or its dialog opens on the host desktop.

### Apps service (id 5)

Apps installed or removed inside Android (an app store, PackageInstaller,
`pm install`) get or lose their desktop entry and icons. The system app
(`apps/AppMirrorFeature.kt`) reports launchable, non-system packages; the bridge
writes the entries with `linuxapk-core` (`xdg::install_app`), the same code as
`linuxapk install` (`bridge/src/services/apps.rs`).

| Type     | Message | Body                                                                                                      | Reply                                                                                                  |
| -------- | ------- | --------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `0x0501` | HAVE    | name package, name version                                                                                | `u8`: 0 missing or outdated (send ADDED), 1 current, 2 not ours (leave it)                             |
| `0x0502` | ADDED   | name package, name version, name label, `i32 category` (-1 to 64), `u8 game`, `u32 len`; fd: sealed memfd | (none). The blob is icons: repeated `u32 size`, `u32 len`, PNG (square, ≤ 512, at most 8, ≤ 4 MiB all) |
| `0x0503` | REMOVED | name package                                                                                              | (none)                                                                                                 |

- **Who owns an entry:** entries written here carry `X-LinuxApk-Origin=android`.
  An entry from `linuxapk install` (which knows more, such as URL schemes) is
  never replaced, and an entry whose `Exec` names another session instance is
  never touched. An uninstall inside Android removes this instance's entry,
  whoever wrote it.
- **When:** at each connection the system app checks every launchable user app
  (HAVE), so installs made while the bridge was away catch up; then it follows
  `PACKAGE_ADDED`/`REPLACED`/`CHANGED`/`FULLY_REMOVED`.
- **Where:** `$XDG_DATA_HOME` (`applications/dev.linuxapk.<pkg>.desktop`,
  `icons/hicolor/<n>x<n>/apps/`). `Exec` is `linuxapk run` (with the session
  instance, as `linuxapk install` writes it).
- **Off by default except in the main session:** the bridge mirrors only with
  `--mirror-apps`, which the launcher passes for the default instance or with
  `LINUXAPK_MIRROR_APPS=1` (`0` turns it off), so lanes and the compat runner
  don't fill the user's app menu. Otherwise every request succeeds and does
  nothing.

Proof (2026-10-27, nested KWin with plasmashell, CI image, instance with
`LINUXAPK_MIRROR_APPS=1` and a test `XDG_DATA_HOME`): `pm install` of F-Droid
wrote `dev.linuxapk.org.fdroid.fdroid.desktop` and its icons. In F-Droid,
Install on Editor showed PackageInstaller's "Install this app?" inside F-Droid's
window (the install-from-this-source app op was granted beforehand); confirming
it wrote Editor's entry, which Kickoff listed as "Editor — Android app
(org.billthefarmer.editor)" and launched (`linuxapk run`) into a window of its
own. `pm uninstall` removed the entry and its icons.
