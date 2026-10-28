# Input: host seat to Android input events

"Linux.apk System" (`dev.linuxapk.system`, sources in `android/`) is our
privileged system app. Its first feature reads the host seat from the bridge's
input service (lines in `host/bridge/src/input/proto.rs`;
[bridge.md](bridge.md#input-service-id-6)) and injects real `MotionEvent`s and
`KeyEvent`s into InputDispatcher. Later desktop-integration features (dark mode,
notifications, file pickers, clipboard; M3) go into the same app as more
`Feature`s.

## Build and install

```sh
android/build.sh            # unit tests, build, sign, install
android/build.sh --no-test
```

- **Toolchain:** Gradle wrapper (9.8.2), AGP 9.4.1 with built-in Kotlin
  (2.4.21), JDK from mise (`core:java` zulu 25), against SDK
  `platforms/android-37.0` and build-tools 37.0.0. Kotlin is formatted with
  ktfmt through dprint (`mise run fix`).
- **Modules:**
  - `android/input`: pure Kotlin/JVM. The protocol parser, the keymap and the
    translator live here. It compiles against `android.jar` only for its
    constants, which are inlined, so its tests run on a plain JVM.
  - `android/app`: the APK. It holds the socket client, the injector and the app
    shell.
- **Signing:** AGP leaves the release APK unsigned. `build.sh` signs it with
  AOSP's public test-keys platform key. The key isn't committed: `build.sh`
  copies it from the AOSP checkout or fetches it from android.googlesource.com
  into `out/keys/`, and checks its SHA-256 either way. The emulator image is
  signed with that key too (its certificate SHA-256 is `c8a2e9bc…`, the same as
  `framework-res.apk`). To use another key, set `LINUXAPK_PLATFORM_KEY`.
- **Install:** `build.sh` writes two files under `out/system_ext-overlay/`:
  - `priv-app/LinuxApkSystem/LinuxApkSystem.apk`
  - `etc/permissions/dev.linuxapk.system.xml`

  `session/ns-init.sh` layers `out/system_ext-overlay` over `/system_ext`, the
  same way `out/vendor-overlay` is layered over `/vendor`.

- **Permissions XML:**
  - an empty `privapp-permissions` entry. The image has
    `ro.control_privapp_permissions=enforce`, so any privileged permission we
    add later must be listed there, or system_server won't boot.
  - a gid mapping for our own permission (see "Needed from other lanes").
- **Keymap:** `android/tools/gen-keymap.py` generates `EvdevKeymap.kt` from
  AOSP's `Generic.kl` (286 keys).

The manifest sets `persistent="true"` and `directBootAware="true"`.
ActivityManager starts persistent apps at `systemReady`, before
`boot_completed`, and restarts the app if it dies. It requests `INJECT_EVENTS`,
which is a signature permission. Hidden APIs are open to platform-signed apps
(`ApplicationInfo.isAllowedToUseHiddenApis`), so
`InputManagerGlobal.injectInputEvent` and `MotionEvent.setActionButton` are
called by reflection.

The lines come from the bridge's input service, which only the app's uid may
open (`input/HostInputClient.kt`). `persist.linuxapk.input.escape_back` (default
`false`, read when the feature starts) sends BACK for Escape.

## Mapping

The mapping follows what InputReader produces for real devices
(`CursorInputMapper`, the multi-touch mapper, `KeyboardInputMapper`).

- **Pointer (`SOURCE_MOUSE`, `TOOL_TYPE_MOUSE`):** events are grouped by
  `pointer_frame`. Each frame emits, in this order:
  1. a move: `HOVER_MOVE`, or `MOVE` while a button is held;
  2. `BUTTON_RELEASE` for each released button;
  3. `UP` or `DOWN` when the pointer goes up or down;
  4. `BUTTON_PRESS` for each pressed button;
  5. `HOVER_MOVE` after an `UP`;
  6. `SCROLL`.

  Left, right and middle map to PRIMARY, SECONDARY and TERTIARY. `downTime` is
  the time of the `DOWN`. If the pointer leaves the window while a button is
  held, the buttons are released (Wayland sends no release after `leave`), and
  then `HOVER_EXIT`. The side buttons BTN_SIDE/BACK and BTN_EXTRA/FORWARD become
  `KEYCODE_BACK` and `KEYCODE_FORWARD`, which is what InputReader synthesizes.

- **Scroll:** a wheel with `value120` gives one unit of `AXIS_VSCROLL` or
  `AXIS_HSCROLL` per detent. Smooth (touchpad) scrolling is converted from
  surface pixels to display pixels, then divided by the framework's
  `config_verticalScrollFactor` (64 dp), so content follows the fingers. Signs:
  Wayland down is VSCROLL −1; Wayland right is HSCROLL +1. As in
  CursorInputMapper, a zero-length `HOVER_MOVE` comes first if the pointer isn't
  hovering yet.
- **Touch (`SOURCE_TOUCHSCREEN`, `TOOL_TYPE_FINGER`):** events are grouped by
  `touch_frame`. Each frame emits one `MOVE` with every pointer, then a `DOWN`
  or `POINTER_DOWN` per new finger, then a `POINTER_UP` or `UP` per lifted
  finger. Host ids are mapped to the lowest free Android pointer id, so pointers
  are sorted by id. `touch_cancel` becomes `CANCEL`.
- **Keyboard (`SOURCE_KEYBOARD`, `FLAG_FROM_SYSTEM`):**
  - Keycodes come from Generic.kl, and the scan code is the evdev code.
  - Meta state is tracked from the modifier keys, with left and right bits, as
    KeyboardInputMapper does. Modifiers held before the window got focus, and
    latched modifiers, come from the xkb depressed and latched masks. Caps Lock
    and Num Lock come from the xkb locked mask. Pointer events carry the same
    meta state.
  - On `keyboard_leave`, held keys are released with `FLAG_CANCELED`.
- **Key repeat:** Wayland never sends repeats. InputDispatcher synthesizes them
  for injected key downs, which are trusted, using Android's own repeat timeout
  and delay. We inject only down and up.
- **Escape stays `KEYCODE_ESCAPE`.** Android 17 already gives Escape desktop
  behaviour: dialogs, popups and text selection close on it. An unhandled Escape
  closes system dialogs (`KeyGestureController`). Activities that close on an
  outside touch finish on it. Mapping Escape to BACK would break apps and games
  that use Escape, and would navigate back where desktop users expect "cancel".
  The mouse back button and the keyboard's Back key send BACK. Set
  `persist.linuxapk.input.escape_back=1` to get BACK on Escape.
- **Timestamps:** the host's u32 millisecond clock is mapped onto
  `uptimeMillis`. The mapping keeps the spacing between events, which
  VelocityTracker needs for flings. It is clamped to `[now − 100 ms, now]`,
  never goes backwards, and handles the u32 wrap.
- **Windows (protocol v2):** each window is an Android display
  ([windowing.md](windowing.md)). The display id on `pointer_enter`,
  `keyboard_enter` and `touch_down` decides where the following events go; they
  are injected with `InputEvent.setDisplayId`. When the pointer moves to another
  window, the old display first gets its button releases and `HOVER_EXIT`. A
  touch sequence stays on the display of its first finger, and fingers that land
  on another window meanwhile are dropped. Smooth-scroll distances on a virtual
  display convert with the window's scale.
- **One injection stream:** InputDispatcher gives every injected event device id
  −1 and checks the stream with InputVerifier. The mouse and the touchscreen
  therefore must not overlap. A touch sequence that starts during a mouse drag
  is dropped, as is mouse input during a touch sequence. A touch ends the mouse
  hover with `HOVER_EXIT`.
- **Disconnects:** if the composer goes away, the client releases everything
  held and reconnects with backoff from 250 ms to 5 s.

## Text input

"Linux.apk Keyboard" (`ime/HostImeService.kt`, an `InputMethodService` in the
system app) connects Android text fields to the host's keyboard and input
methods through the bridge's `zwp_text_input_v3`
([bridge.md](bridge.md#text-input)). The pure logic, which is unit-tested on the
JVM, is in `android/input/.../ime/TextInputSession.kt`.

- **Default, no soft keyboard:** `ImeFeature` enables it and makes it the
  default input method at every start, unless the user chose another one than
  the stock LatinIME. It never shows an input view, so nothing pops up on focus.
  The window manager's per-display IME policy is now `local`: with `hide`,
  InputMethodManager binds no IME at all (`NO_IME`), so there would be no text
  input either.
- **Keys:** injected key events still reach the app as before (the IME passes
  them through untouched), so typing, shortcuts, Backspace and Enter don't
  change. Without a host input method, that is all there is.
- **With a host input method** (IBus, Fcitx; this covers CJK, dead keys and
  compose):
  - For each editor (`onStartInput`, any input type but `TYPE_NULL`) the IME
    sends `ti_enable` and the content type (input type and IME options mapped to
    the protocol's hints and purpose: password, email, URL, number, PIN, phone,
    date, multi-line, capitalization).
  - It keeps the bridge up to date with the surrounding text (up to 120 UTF-16
    units each side of the selection, trimmed to fit a line) and the cursor
    rectangle (`requestCursorUpdates`; the insertion marker through
    `CursorAnchorInfo`'s matrix, in display pixels). The host input method's
    candidate window opens at the text cursor.
  - The input method's results come back as `ti_*` lines on the composer's
    stream and are applied in `zwp_text_input_v3`'s order inside one batch edit:
    remove the preedit, `deleteSurroundingTextInCodePoints` (byte lengths
    converted with the text last sent), `commitText`, `setComposingText` for the
    new preedit.
  - A selection change caused by those edits is reported with change cause
    `input_method`; any other one with `other`, which resets the input method.
- **Limits:** `setComposingText` can only put the cursor at either end of the
  preedit, so a cursor inside it (Anthy's segment editing) shows at the end.
  `TYPE_NULL` editors (terminals) get raw keys only.

Proof (2026-10-27, nested KWin 6.7 at scale 1.7 with IBus 1.5.34 as KWin's input
method, CI image): in F-Droid's search field, IBus Pinyin's preedit "你好"
showed underlined in the field with the candidate window under the text cursor,
and Space committed it (the field's text was `你好`). With the `xkb:us:intl:eng`
engine, `'` then `e` typed `café`. The IME window stayed without a surface on
the app's display (`mViewVisibility=GONE`), so no soft keyboard appeared.
`android/e2e/e2e.sh` checks that the Linux.apk Keyboard is the default.

### Known limits (injection rather than an input device)

- **No keyboard device:** Android sees no physical keyboard. The Linux.apk
  Keyboard (above) keeps the soft keyboard away; apps that look for a hardware
  keyboard (`Configuration.keyboard`) still see none.
- **US layout only:** characters come from `Virtual.kcm`, so the host's xkb
  layout is ignored.
- **No cursor feedback:** Android draws no pointer for injected events, and
  pointer-icon changes (I-beam, hand) aren't forwarded. The host cursor is the
  only cursor.
- **Touchpad gestures:** these are plain scroll events, with no fling or pinch.
- **Rejected alternatives:**
  - uinput isn't available rootless. It would also create a host-global input
    device that the host compositor would pick up.
  - VirtualDeviceManager's virtual input devices sit on uinput too.

## Tests

| What                                                                                                                                                                                                   | How                       | Result                                 |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------- | -------------------------------------- |
| Parser (v1 and v2), keymap, translator (incl. per-display routing); model of AOSP's InputVerifier (per display) run over every test, plus 200 random streams × 300 events (all 14 mouse/touch actions) | `android/build.sh`        | 41 tests, 0 failures                   |
| Real framework classes: `app_process` builds every event from `tests/e2e/system-app/probe-stream.txt` in the live session                                                                              | `StdinInjector --dry-run` | MotionEvent/KeyEvent dumps as expected |

### Proof (2026-10-27, live session, system_server up to NetworkManagementService)

- **Injection into the real InputDispatcher.** Ran
  `CLASSPATH=…apk app_process / dev.linuxapk.system.input.InputBridgeMain --stdin`
  as root with `probe-stream.txt`:
  - All 19 events got `inject=true`, and there was no `Inconsistent event` in
    logcat. They were then dropped with "input dispatch is disabled", because
    WindowManager doesn't enable dispatch until later in boot.
  - Control: `input touchscreen motionevent UP 10 10`, a lone UP, gets
    `E InputDispatcher: Inconsistent event … ACTION_UP`. The verifier is live,
    and our stream passes it.
- **Socket client.** With a toybox `nc -lU` server serving the stream twice, the
  client logged `connected`, `input stream closed`, then reconnected and
  replayed. The reset after each disconnect emitted `HOVER_EXIT`.
- **Package scan.** In a throwaway session (its own unit, out and data dirs),
  `/system_ext/priv-app/LinuxApkSystem` was scanned. packages.xml records it as
  SYSTEM, PERSISTENT, PRIVILEGED, SYSTEM_EXT, SIGNED_WITH_PLATFORM_KEY and
  DIRECT_BOOT_AWARE, with the platform certificate. `HOST_INPUT` is registered
  as a signature permission owned by the app. SystemConfig read the permissions
  XML. That session's system_server never reached `systemReady` (on a fresh
  `/data`, SettingsProvider's database was missing), so the persistent start is
  still unproven.

## End-to-end test (run once the session reaches `sys.boot_completed=1`)

1. Run `android/build.sh`. The session's ns-init layers
   `out/system_ext-overlay`.
2. Start the session with the composer's Wayland backend. This needs the session
   changes below.
3. Run `android/e2e/e2e.sh`. In per-app window mode it opens the probe in a
   window of its own and rewrites the stream to protocol v2 with the probe's
   display id (see [windowing.md](windowing.md)). It checks the following and
   exits non-zero on any FAIL:
   - the package is a system_ext priv-app;
   - `INJECT_EVENTS` is granted;
   - the persistent process runs and logged `started input`;
   - it connected to the bridge's input service.

   It then opens `InputProbeActivity` and injects `probe-stream.txt` through the
   app's translator and injector. It expects the activity's log
   (`LinuxApkProbe`) to show:
   - mouse hover, click and button press (`source=0x2002`);
   - wheel scroll;
   - Shift+A with `META_SHIFT_ON`;
   - Escape;
   - a two-finger `POINTER_DOWN` (`source=0x1002`);
   - no `Inconsistent event` from the dispatcher.

4. Manual check with real hardware:
   1. `session/linuxapk-session exec am start -n dev.linuxapk.system/.input.InputProbeActivity`.
   2. Over the "Linux.apk" window, move the mouse, click, scroll, type, and
      press Escape.
   3. `session/linuxapk-session exec logcat -s LinuxApkProbe LinuxApkInput`
      shows each event. The probe window also shows the last one.
   4. `dumpsys input` lists the recent events under `RecentQueue`.

## Needed from other lanes

- **Session:** the Wayland backend in the full session.
  - Bind the host's `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` socket file into the
    root, as the old `hal/tests/run-android.sh -w` did, and point
    `vendor.linuxapk.wayland.socket` at it.
  - Set `vendor.linuxapk.hwc.backend=wayland`, for example from the launcher's
    `vendor_props`.
- **Bridge (input service):**
  1. Forward these protocol additions (version 2):
     - `axis_source` (wheel or finger), so touchpad scrolling isn't guessed from
       a missing `value120`;
     - `repeat_info <rate> <delay>`, so the bridge can set Android's key repeat
       settings;
     - the keymap or keysyms, for non-US layouts.
  2. Set a host cursor on `wl_pointer.enter`, through `wp_cursor_shape_v1` or a
     cursor surface. Today the cursor over the window is undefined.
- **With SELinux enforcing** (our own build, later): the app's domain needs
  `connectto` on the composer's socket.
