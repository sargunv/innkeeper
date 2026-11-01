# Input

The host seat reaches Android through the bridge's input service. Only the
Innkeeper System app opens that service. The bridge turns `wl_pointer`,
`wl_keyboard`, `wl_touch`, touchpad gestures and tablets into protocol lines
([input/proto.rs](../host/bridge/src/input/proto.rs)). The system app
(`input/InputTranslator.kt`) turns those lines into the events InputReader would
produce for real devices.

The system app then sends events down two paths. Modifiers and mouse input go
back through the bridge as evdev records to the broker's virtual devices.
Ordinary keys, touch, pen and touchpad gestures are injected into
InputDispatcher in one ordered stream. That stream keeps the results of the
host's layout, compose and input method intact. `input/VirtualInput.kt` explains
the split.

An OpenHarmony window gets the seat from the bridge as a touchscreen and a
keyboard ([ohos](../ohos/README.md#input)).

## Devices

The broker serves an Innkeeper Keyboard, one Innkeeper Mouse per window and one
device per host controller, and an Innkeeper Touchscreen for OpenHarmony. All of
them are private to the guest. EventHub's `/dev/input` is redirected to a broker
directory, so EventHub sees only these devices, and their records arrive over
`evdev.sock`. Innkeeper doesn't use uinput, because uinput creates host-global
devices that the compositor picks up, and VirtualDeviceManager is built on
uinput. Apps see a real keyboard (`Configuration.keyboard` is QWERTY) and one
mouse per window, tied to that window's display. Mouse motion follows the host's
absolute coordinates, which the host has already accelerated, so Android's
pointer scaling is off.

**Controllers.** The bridge reads the host gamepads that the user's ACLs already
allow, through Flatpak's `--device=input`
([security.md](security.md#the-sandbox)). It doesn't grab them, and it looks for
new ones every 250 ms. Buttons and axes map to Android's `Generic.kl` layout.
Sony's north and west buttons map to Y and X. A controller keeps its Android
device across reconnects when its serial number or port identifies it. Rumble is
offered when the source device supports `FF_RUMBLE`. The broker forwards effects
to the host device, and the host device's fd never enters an Android process.

## Translation

- **Per window.** Pointer, keyboard and touch focus decide which display gets
  the events. Moving to another window ends the hover or drag on the old window.
  A touch sequence stays on the window of its first finger.
- **Touch, pen and touchpad gestures** share one injection stream, so they don't
  overlap with each other or with a mouse drag. Overlapping input is dropped.
- **Wheel.** A detent scrolls one unit, and high-resolution wheels give
  fractions. Continuous sources such as trackpoints follow
  `config_verticalScrollFactor`.
- **Touchpads** behave like touchpads on Android. A two-finger scroll is a fake
  finger, so apps fling from its velocity, because Wayland has no kinetic
  scrolling. A hold stops the fling. A pinch is two fake fingers. Swipes of
  three or more fingers go to system gesture monitors and not to apps.
- **Pen.** A tablet tool is a stylus or eraser with hover, pressure, tilt,
  orientation and its two buttons.
- **Key repeat** uses the desktop's delay and rate, which the system app writes
  to Android's key repeat settings. A compositor that repeats keys itself (KWin)
  sends the repeats.
- **Window shortcuts.** Ctrl+W and Ctrl+Q close the window, and Alt+Left sends
  BACK. These shortcuts don't reach the app, except that web browsers get Ctrl+W
  to close a tab. F11 toggles fullscreen.
  `persist.innkeeper.input.window_shortcuts=0` turns off Ctrl+W, Ctrl+Q and
  Alt+Left.
- **Escape stays Escape**, because Android already closes dialogs on it and apps
  use it. The title bar's ‹ button, the mouse back button and the Back key send
  BACK. The manager has a per-app **Escape as Back** switch.

## Keyboard layout

The bridge keeps the compositor's xkb keymap and state (`input/xkb.rs`). It
sends each key press with its level-1 keysym and the text it types, including
composed text. The system app (`Keyboard.kt`) handles keys the way Mac.apk's
host keymap does:

- **Keycodes.** A key whose level-1 keysym is a Latin letter, a digit or Android
  punctuation gets that keycode, so shortcuts follow the layout. For example,
  the AZERTY Z key is `KEYCODE_Z`. Other keys keep their physical keycode, so
  Cyrillic layouts keep the QWERTY shortcut positions.
- **Text.** Injected keys type through Android's US `Virtual.kcm`. When the host
  types something else, such as é, AltGr characters or composed text, the
  Innkeeper Keyboard commits the host's text instead. It can do this because it
  sees every key first. Ctrl and Meta chords never produce text.
- A host input method (IBus, Fcitx) handles compose itself. Keys that it
  consumes don't reach the bridge.

## Text input

The Innkeeper Keyboard (`ime/HostImeService.kt`) connects Android text fields to
the host's input method through the bridge's `zwp_text_input_v3`. It is the
default input method unless the user chose another one. It draws nothing, so no
soft keyboard covers the window. For each editor it sends the content type, the
surrounding text and the cursor rectangle, so the candidate window opens at the
text cursor. It applies preedit text and commits in the order the protocol
gives. It can put the cursor only at either end of a preedit. `TYPE_NULL`
editors, such as terminals, get raw keys only.

## Pointer capture

`View.requestPointerCapture` locks the host pointer on the focused window
(`zwp_pointer_constraints_v1`). The bridge then sends unaccelerated
`zwp_relative_pointer_v1` motion as relative mouse reports. The system app
detects capture when the mouse switches to `SOURCE_MOUSE_RELATIVE`, which is the
public way to observe it. Escape, focus loss or the app's release unlocks the
pointer. It locks again only when the app asks again. An app that hides its
cursor doesn't lock the host pointer.

## Known limits

- Keys injected the ordinary way have device id −1. Apps that read
  `KeyEvent.getUnicodeChar()` outside a text field see US characters.
- Window shortcuts are on or off for all apps at once.
- Views that accept only touchscreen input ignore the touchpad's fake fingers,
  as they do on Android with a touchpad.
- Compositors keep most three- and four-finger swipes for themselves.
- Tablet pads aren't forwarded.

## Tests

The `:protocol` unit tests cover the parser, the keymap and the translator,
using a model of InputVerifier. `cargo test -p innkeeper-bridge` resolves keys
under real keymaps. `mise run test:e2e -- input` checks the common path in a
real window. The IME's commits and the key repeat settings are checked by hand
on a nested desktop
([development.md](development.md#a-nested-isolated-desktop)).
