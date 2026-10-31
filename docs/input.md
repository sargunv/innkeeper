# Input

The host seat reaches Android through the bridge's input service, which only the
Innkeeper System app opens. The bridge turns `wl_pointer`, `wl_keyboard`,
`wl_touch`, touchpad gestures and tablets into protocol lines
([input/proto.rs](../host/bridge/src/input/proto.rs)); the system app
(`input/InputTranslator.kt`) turns them into what InputReader would produce for
real devices. Modifiers and mouse input then go back through the bridge as evdev
records to the broker's virtual devices; ordinary keys, touch, pen and touchpad
gestures are injected into InputDispatcher, in one ordered stream that keeps the
host's layout, compose and input-method results intact (`input/VirtualInput.kt`
says why the split).

## Devices

The broker serves an Innkeeper Keyboard, one Innkeeper Mouse per window and one
device per host controller, all private to Android: EventHub's `/dev/input` is
redirected to a broker directory, so it sees only these, and records arrive over
`evdev.sock`. uinput would create host-global devices the compositor picks up,
and VirtualDeviceManager sits on uinput too. Apps see a real keyboard
(`Configuration.keyboard` is QWERTY) and a mouse per window, tied to that
window's display. Mouse motion follows the host's absolute, already accelerated
coordinates, so Android's own scaling is off.

**Controllers.** The bridge reads host gamepads the user's ACLs already allow
(Flatpak's `--device=input`; [security.md](security.md#the-sandbox)), without
grabbing them, and rediscovers them every 250 ms. Buttons and axes map to
Android's `Generic.kl` layout, Sony's north and west buttons to Y and X. A
controller keeps its Android device across reconnects where its serial or port
allows. Rumble is offered when the source supports `FF_RUMBLE`; the broker
forwards effects to the host device, whose fd never enters an Android process.

## Translation

- **Per window.** Pointer, keyboard and touch focus decide the display events go
  to. Moving to another window ends the hover or drag on the old one; a touch
  sequence stays on its first finger's window.
- **Touch, pen and touchpad gestures** share one injection stream, so they never
  overlap with each other or a mouse drag; overlapping input is dropped.
- **Wheel.** A detent is one unit of scroll, hi-res wheels give fractions, and
  continuous sources (trackpoints) follow `config_verticalScrollFactor`.
- **Touchpads** behave as Android's own do: two-finger scrolling is a fake
  finger, so apps fling from its velocity (Wayland has no kinetic scrolling),
  and a hold stops the fling; pinch is two fake fingers; swipes of three or more
  fingers go only to system gesture monitors, never apps.
- **Pen.** A tablet tool is a stylus or eraser with hover, pressure, tilt,
  orientation and its two buttons.
- **Keys repeat** at the desktop's delay and rate, written to Android's key
  repeat settings; a compositor that repeats keys itself (KWin) sends repeats.
- **Window shortcuts.** Ctrl+W and Ctrl+Q close the window and Alt+Left sends
  BACK; none reaches the app, except Ctrl+W in web browsers, which closes a tab.
  F11 toggles fullscreen. `persist.innkeeper.input.window_shortcuts=0` turns the
  first three off.
- **Escape stays Escape**: Android already closes dialogs on it, and apps use
  it. The title bar's ‹, the mouse back button and the Back key send BACK; the
  manager's per-app **Escape as Back** switch opts in.

## Keyboard layout

The bridge keeps the compositor's xkb keymap and state (`input/xkb.rs`) and
sends each press with its level-1 keysym and the text it types, compose
included. The system app (`Keyboard.kt`) follows Mac.apk's approach:

- **Keycodes.** A key whose level-1 keysym is a Latin letter, a digit or Android
  punctuation gets that keycode, so shortcuts follow the layout (AZERTY Z is
  `KEYCODE_Z`); other keys keep their physical keycode, so Cyrillic layouts keep
  QWERTY shortcut positions.
- **Text.** Injected keys type through Android's US `Virtual.kcm`. When the host
  types something else (é, AltGr characters, composed text), the Innkeeper
  Keyboard, which sees every key first, commits the host's text instead. Ctrl
  and Meta chords are never text.
- A host input method (IBus, Fcitx) handles compose itself; keys it consumes
  never reach the bridge.

## Text input

The Innkeeper Keyboard (`ime/HostImeService.kt`) connects Android text fields to
the host's input method through the bridge's `zwp_text_input_v3`. It is the
default input method unless the user chose another, and draws nothing, so no
soft keyboard covers the window. For each editor it sends the content type, the
surrounding text and the cursor rectangle, so the candidate window opens at the
text cursor, and applies preedit and commits in the protocol's order. It can
only put the cursor at either end of a preedit, and `TYPE_NULL` editors
(terminals) get raw keys only.

## Pointer capture

`View.requestPointerCapture` locks the host pointer on the focused window
(`zwp_pointer_constraints_v1`) and sends unaccelerated `zwp_relative_pointer_v1`
motion as relative mouse reports. The signal is the mouse switching to
`SOURCE_MOUSE_RELATIVE`, the public way to observe capture. Escape, focus loss
or the app's release unlock it, and it locks again only when the app asks again.
An app hiding its cursor never locks the host pointer.

## Known limits

- Ordinarily injected keys have device id −1, and apps that read
  `KeyEvent.getUnicodeChar()` outside a text field see US characters.
- Window shortcuts are on or off for every app at once.
- Views that accept only touchscreen input ignore the touchpad's fake fingers,
  as on Android with a touchpad.
- Compositors keep most three- and four-finger swipes, and tablet pads aren't
  forwarded.

## Tests

`:protocol` unit tests cover the parser, keymap and translator (with a model of
InputVerifier); `cargo test -p innkeeper-bridge` resolves keys under real
keymaps. `mise run test:e2e -- input` checks the common path in a real window.
The IME's commits and repeat settings are checked by hand on a nested desktop
([development.md](development.md#a-nested-isolated-desktop)).
