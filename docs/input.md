# Input

The host seat reaches Android through the bridge's input service, which only the
Linux.apk System app may open. The bridge turns `wl_pointer`, `wl_keyboard`,
`wl_touch`, touchpad gestures (`zwp_pointer_gestures_v1`) and tablets
(`zwp_tablet_v2`) into protocol v2 lines
([input/proto.rs](../host/bridge/src/input/proto.rs) lists every line); the
system app translates them into `MotionEvent`s and `KeyEvent`s and injects them
into InputDispatcher. The composer never sees input.

```mermaid
flowchart LR
  seat["host seat<br/>wl_pointer, wl_keyboard, wl_touch,<br/>gestures, tablets"] --> bseat["bridge input/seat.rs<br/>gestures.rs, tablet.rs"]
  im["host input method<br/>IBus, Fcitx"] <-->|"zwp_text_input_v3"| bti["bridge input/text_input.rs"]
  bseat -->|"INPUT lines"| hic["HostInputClient"]
  bti -->|"ti_* lines"| hic
  hic --> tr["InputTranslator (:protocol)"] --> inj["Injector (:inject)"] --> disp["InputDispatcher"]
  hic --> ime["HostImeService"]
  ime -->|"TEXT_INPUT"| bti
```

## Injection

The mapping follows what InputReader produces for real devices
(`CursorInputMapper`, the multi-touch mapper, `TouchInputMapper`'s stylus,
`GestureConverter` for touchpads, `KeyboardInputMapper`); the code is
`android/system-app/protocol/.../input/InputTranslator.kt`.

- **Per window.** The display id on `pointer_enter`, `keyboard_enter` and
  `touch_down` decides where the following events go (`setDisplayId`). Moving to
  another window first ends the hover or drag on the old one; a touch sequence
  stays on its first finger's window.
- **One injection stream.** InputDispatcher gives every injected event device id
  −1 and checks the stream with InputVerifier, so the mouse, touchscreen and pen
  must never overlap: touch or a pen during a mouse drag or touchpad gesture is
  dropped, and so is mouse input during a touch sequence or while the pen is in
  proximity. Leaving a window with a button held releases it (Wayland sends no
  release after `leave`).
- **Wheel.** A detent (`value120` 120) is one unit of `AXIS_VSCROLL` or
  `AXIS_HSCROLL`; hi-res wheels give fractions, as CursorInputMapper's hi-res
  scroll does. Continuous sources (trackpoints) convert surface pixels to
  display pixels and divide by `config_verticalScrollFactor`.
- **Touchpad scrolling** (`axis_source` finger) is what Android 14+ does for its
  own touchpads (GestureConverter): the hover exits, and a fake finger
  (`SOURCE_MOUSE`, `TOOL_TYPE_FINGER`, `CLASSIFICATION_TWO_FINGER_SWIPE`, the
  `AXIS_GESTURE_SCROLL_*_DISTANCE` axes) goes down at the cursor, moves with the
  content and lifts at `axis_stop`. The app flings from the finger's own
  velocity; Wayland sends no kinetic scrolling. A hold gesture after a scroll
  sends a fake finger down and cancels it, which stops the fling.
- **Touchpad pinch and swipes** are GestureConverter's too: a pinch is two fake
  fingers 200 px apart around the cursor (`CLASSIFICATION_PINCH`,
  `AXIS_GESTURE_PINCH_SCALE_FACTOR`) that spread with the scale, turn with the
  rotation and follow the center; a swipe of three or more fingers is fake
  fingers 100 px apart (`MULTI_FINGER_SWIPE`), which InputDispatcher gives only
  to system gesture monitors, never to apps, as with Android's own touchpads. A
  cancelled gesture is `ACTION_CANCEL`.
- **Pen.** A tablet tool in proximity is `SOURCE_STYLUS | SOURCE_TOUCHSCREEN`
  with `TOOL_TYPE_STYLUS` or `TOOL_TYPE_ERASER`: `HOVER_*` with `AXIS_DISTANCE`,
  `DOWN`/`MOVE`/`UP` with pressure (the lift repeats the last contact),
  `AXIS_TILT` and `AXIS_ORIENTATION` from the two tilt angles, and the stylus
  buttons as `BUTTON_STYLUS_PRIMARY` and `SECONDARY`, in TouchInputMapper's
  order.
- **Cursor.** The bridge sets the default arrow on every pointer enter and
  tablet proximity, and the frame's resize shapes over a client-side frame
  (`wp_cursor_shape_v1`).
- **Keys.** Keycodes come from AOSP's `Generic.kl`
  (`android/system-app/tools/gen-keymap.py` generates `EvdevKeymap.kt`), with
  the evdev code as scan code. Meta state comes from the modifier keys and the
  xkb masks. Wayland sends no repeats; InputDispatcher synthesizes them for
  injected key downs.
- **Escape stays Escape.** Android 17 already closes dialogs and popups on it;
  mapping it to BACK would break apps that use Escape. The mouse back button and
  the Back key send BACK; `persist.linuxapk.input.escape_back=1` opts in.
- **Timestamps** map the compositor's millisecond clock onto `uptimeMillis`,
  keeping the spacing VelocityTracker needs, clamped to the last 100 ms.
- **Why injection.** uinput isn't available rootless and would create a
  host-global device the compositor picks up; VirtualDeviceManager's input
  devices sit on uinput too.

## Text input

"Linux.apk Keyboard" (`ime/HostImeService.kt`) connects Android text fields to
the host's input methods through the bridge's `zwp_text_input_v3`; its pure
logic is `TextInputSession.kt` in `:protocol`.

- **Default, never shown.** `ImeFeature` makes it the default input method at
  every start unless the user chose another than the stock LatinIME. It draws no
  input view, so no soft keyboard covers the window. The window manager's
  per-display IME policy is `LOCAL`: `HIDE` would bind no IME at all, and text
  input would stop.
- **Keys** still arrive as injected key events, which the IME passes through, so
  without a host input method nothing changes.
- **With a host input method** (CJK, dead keys, compose): for each editor the
  IME sends `ti_enable`, the content type (password, email, number, …), the
  surrounding text (up to 120 UTF-16 units each side) and the cursor rectangle,
  so the candidate window opens at the text cursor. The input method's results
  come back as `ti_*` lines and are applied in `zwp_text_input_v3`'s order in
  one batch edit: remove the preedit, delete surrounding text, commit, set the
  new preedit.
- **Limits:** `setComposingText` can only put the cursor at either end of the
  preedit; `TYPE_NULL` editors (terminals) get raw keys only.

## Known limits

- Android sees no physical keyboard (`Configuration.keyboard`), and characters
  come from `Virtual.kcm`, so the host's xkb layout is ignored (US only).
- Android draws no pointer for injected events, and pointer-icon changes aren't
  forwarded; the host cursor is the only cursor, and it stays the arrow (text
  fields show no I-beam, games can't hide it). Without `wp_cursor_shape_v1` the
  cursor over a window is whatever it was.
- Views that accept only `SOURCE_TOUCHSCREEN` ignore the touchpad's fake
  fingers, as on stock Android with a touchpad.
- Compositors keep most three- and four-finger swipes for themselves, and tablet
  pads (buttons, rings, strips) aren't forwarded.

## Tests

- `:protocol` unit tests (in `mise run test`): the line parser, keymap and
  translator, with a model of AOSP's InputVerifier run over every test plus
  random streams.
- `tests/e2e/system-app/run.sh` (in `mise run test:e2e`): opens the probes APK's
  `InputProbeActivity` in its own window, injects `probe-stream.txt` through the
  app's translator and injector, and checks the events (hover, click, scroll,
  Shift+A, Escape, two fingers, touchpad scroll, fling stop, pinch, pen and
  eraser) and that the dispatcher reported no inconsistent event.
