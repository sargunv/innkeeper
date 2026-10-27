# Desktop integration inventory

Every Android system API, service, input type and platform feature that has, or
should have, a desktop Linux counterpart. It's the M3 checklist, and it's meant
to be kept current: when a lane lands a row, update its status and pointers
here. Researched 2026-10-27; sources are in the appendix.

## How to read this

Each row is one checklist item:

- **Status:** `done`, `partial`, `todo` or `won't-do`, with the reason.
- **Priority:**
  - `P0`: typical apps need it;
  - `P1`: important;
  - `P2`: nice to have.
- **Lane** (owner):
  - `input`, `windowing`, `bridge`, `sysapp` (the "Linux.apk System" app),
    `audio`, `hal` (composer and other vendor HALs);
  - `session`, `image` (our source-built image, overlays, config), `cli`,
    `compat`.
- **Now:** what exists today, with file pointers. Paths on `lane/<name>` aren't
  on `main` yet.
- **Linux:** the desktop side: a Wayland protocol, an xdg-desktop-portal
  interface, a D-Bus service, PipeWire and so on.
- **Android:** what we hook and how: a HAL, the privileged system app, an
  overlay or config, or an intent handler.
- **Test:** a FOSS app (F-Droid unless noted) that exercises the row, for the
  compat lane. "(verify)" means we haven't confirmed the app uses that feature.

## Architecture this assumes (decided 2026-10-27)

- **Host `bridge/` daemon** (Rust, glibc, outside the namespaces). It owns:
  - the Wayland client: one `xdg_toplevel` per Android virtual display, with SSD
    or CSD;
  - audio through PipeWire;
  - every portal and D-Bus integration.

  The first version is on `lane/audio`: `bridge/` (a seqpacket protocol and
  sealed memfd rings). Today the composer HAL is still the Wayland client
  (`hal/composer/WaylandPresenter.cpp`). The rows below give the target owner.

- **"Linux.apk System"** (`dev.linuxapk.system`, `android/`). A Kotlin,
  platform-signed, persistent priv-app. It injects input, and on `lane/win` it
  is the window manager (`android/app/.../wm/WindowManagerFeature.kt`). Every
  Android-side integration below is a `Feature` in this app unless it says HAL
  or overlay. Privileged permissions must be listed in
  `android/etc/permissions/dev.linuxapk.system.xml`
  (`ro.control_privapp_permissions=enforce`).
- **One socket between them.** The bridge and the system app speak one versioned
  protocol over a socket bound into the session. Host files cross it only as
  passed fds, never as bind-mounted host directories. This keeps the host-safety
  rule: no `$HOME` bind.

### A cross-cutting decision: injected events or virtual input devices

Today input is injected (`InputManagerGlobal.injectInputEvent`, device id −1).
That's enough for pointer, touch and keys, but these rows can't be done well
without a real Android `InputDevice`:

- gamepads, which engines enumerate through `InputDevice`;
- rumble (`InputDevice.getVibratorManager`);
- non-US keymaps (a per-device KCM);
- no soft keyboard while a keyboard is attached;
- touchpad gestures (Android's own gesture stack);
- the stylus;
- pointer icons drawn by PointerChoreographer.

uinput is out: it needs root, and it would create a host-global device. Waydroid
patches EventHub to read FIFOs. Our no-patch equivalent is **virtual evdev nodes
through the seccomp supervisor** (the same mechanism as
`session/helper/bpfemu.c`):

1. `openat("/dev/input/eventN")` gets an injected pipe or socketpair fd
   (`SECCOMP_IOCTL_NOTIF_ADDFD`).
2. The supervisor answers `EVIOCG*` ioctls, and takes `EVIOCSFF`/`EV_FF` writes
   for rumble and `EV_LED` writes.
3. The bridge writes `input_event` records into the fd.

EventHub discovers the devices through inotify on a tmpfs `/dev/input`. The rows
marked "needs virtual evdev" depend on this; effort L. It also overlaps the
ubinder supervisor work.

## Input

### Pointer

- [x] **Buttons, hover, enter/leave** · done · P0 · input
  - Now: `android/input/.../InputTranslator.kt`; stream from
    `hal/composer/InputServer.cpp`; e2e in `android/e2e/e2e.sh`.
  - Linux: `wl_pointer` (enter, leave, motion, button, frame).
  - Android: injected `SOURCE_MOUSE` MotionEvents (hover, `BUTTON_PRESS`).
    BTN_SIDE/EXTRA become BACK/FORWARD.
  - Test: Material Files (context menus, hover).
- [ ] **High-resolution wheel scroll** · partial · P0 · input
  - Now: `value120` gives one `AXIS_VSCROLL` unit per detent. Fractional detents
    are dropped, and `axis_source` is guessed (backlog: input protocol v2).
  - Linux: `wl_pointer.axis_value120` (v8), `axis_source`,
    `axis_relative_direction`.
  - Android: `AXIS_VSCROLL` with fractional values (value120/120), as
    CursorInputMapper does for hi-res wheels.
  - Test: Read You (long lists).
- [ ] **Horizontal and touchpad (smooth) scroll** · partial · P0 · input
  - Now: `AXIS_HSCROLL`. Smooth scroll is converted with
    `config_verticalScrollFactor`, with no kinetic fling.
  - Linux: `wl_pointer.axis` with `axis_source=finger`, `axis_stop`.
  - Android: with injection, synthesize the fling from `axis_stop` velocity.
    With a virtual touchpad (needs virtual evdev), Android's own touchpad stack
    does it.
  - Test: Organic Maps (horizontal pan), Fossify Gallery.
- [ ] **Natural-scroll and per-device settings** · todo · P2 · input
  - Linux: the compositor already applies them. Nothing to forward.
  - Android: make sure Android doesn't reverse them a second time (only an issue
    with a virtual touchpad).

### Keyboard

- [x] **Keys, modifiers, locks** · done · P0 · input
  - Now: `EvdevKeymap.kt` (generated from `Generic.kl`), `Keyboard.kt`; meta
    state comes from the xkb masks.
  - Linux: `wl_keyboard` (key, modifiers, enter, leave).
  - Android: injected `KeyEvent`s with scan codes.
- [ ] **Layouts (non-US)** · todo · P0 · input
  - Now: US only (`Virtual.kcm`); the host xkb layout is ignored.
  - Linux: the `wl_keyboard.keymap` xkb keymap, read with xkbcommon in the
    bridge.
  - Android, two parts:
    1. text goes through our IME's `commitText` (see IME);
    2. for shortcuts, KeyEvents carry the layout-correct keycode from the xkb
       keysym, so Ctrl+Z on AZERTY is `KEYCODE_Z`.

    With virtual evdev, map the xkb layout name to Android's KCM layouts
    (`InputManager.setKeyboardLayoutForInputDevice`, layouts from the
    `InputDevices` package).

  - Test: Markor with a German or French layout.
- [ ] **Compose and dead keys** · todo · P1 · input
  - Linux: xkbcommon compose tables (`xkb_compose_state`) in the bridge, or the
    host IME (fcitx5/IBus) through `text-input-v3`.
  - Android: the composed character is committed through the IME. Android's own
    dead-key handling in KCM only applies with a virtual keyboard device.
  - Test: Markor (é via dead acute, ñ via Compose).
- [x] **Key repeat** · done · P0 · input
  - Now: InputDispatcher synthesizes repeats for injected trusted key downs.
- [ ] **Host repeat rate and delay** · todo · P2 · input
  - Linux: `wl_keyboard.repeat_info`.
  - Android: `Settings.Secure` `key_repeat_timeout`/`key_repeat_delay` (Android
    16+ key repeat settings) from the system app.
- [ ] **Media and special keys** · partial · P1 · input + bridge
  - Now: the keymap has `KEYCODE_MEDIA_*` and volume, but the host compositor
    usually takes these keys before the window does.
  - Linux: media keys reach players through MPRIS (see Media).
  - Android: MediaSession transport controls. Volume keys are host-side.

### Touch and stylus

- [x] **Touch and multi-touch** · done · P0 · input
  - Now: `wl_touch` frames become `SOURCE_TOUCHSCREEN` (`POINTER_DOWN/UP`,
    `CANCEL`), with id remapping.
  - Linux: `wl_touch`.
  - Test: Organic Maps (pinch on a touchscreen).
- [ ] **Pen/stylus: pressure, tilt, buttons, eraser, hover** · todo · P1 · input
  - Linux: `zwp_tablet_v2`:
    - tool type pen, eraser or airbrush;
    - pressure 0–65535, tilt in degrees, distance;
    - BTN_STYLUS/BTN_STYLUS2;
    - proximity in and out for hover.

    Waydroid already maps this.

  - Android: `SOURCE_STYLUS`, `TOOL_TYPE_STYLUS` or `TOOL_TYPE_ERASER`:
    - `AXIS_PRESSURE` (0–1), `AXIS_TILT` and `AXIS_ORIENTATION` (radians, from
      tilt x/y), `AXIS_DISTANCE`;
    - `BUTTON_STYLUS_PRIMARY` and `BUTTON_STYLUS_SECONDARY`;
    - `HOVER_ENTER/MOVE/EXIT` on proximity.

    Injection can carry all of this; it must not overlap the mouse stream (the
    InputVerifier rule in `docs/input.md`). Handwriting-to-text (Android 14
    stylus handwriting) needs the IME to support it. That's P2.

  - Test: Saber (pressure, eraser).
- [ ] **Tablet pad buttons and rings** · todo · P2 · input
  - Linux: `zwp_tablet_pad_v2`.
  - Android: no standard mapping. Use key bindings (see game mapping).

### Touchpad gestures

- [ ] **Pinch to zoom, rotate** · todo · P1 · input
  - Now: touchpad pinch isn't forwarded.
  - Linux: `zwp_pointer_gestures_v1` pinch (scale, rotation). Hold gestures stop
    flings.
  - Android, two options:
    - Inject a synthetic two-finger touch around the pointer, scaled by the
      pinch factor. This is what Mac.apk and Nucleus do (see
      `nucleus/docs/internals/tao-trackpad.md` for the pitfalls). Apps that only
      accept mouse input see a touch.
    - With virtual evdev, a real touchpad device gets Android's gesture library,
      which gives `CLASSIFICATION_PINCH`.
  - Test: Organic Maps, Fossify Gallery.
- [ ] **Three/four-finger swipes** · won't-do · P2 · input
  - The host compositor owns these (desktop switching, overview). Android's
    swipe classifications target its own system UI.

### Gamepads

- [ ] **Controllers as Android InputDevices** · todo · P1 · input + bridge
  - Linux: the bridge reads evdev joysticks the user can access
    (`/dev/input/event*` with the uaccess ACL), or SDL3's gamepad API for
    mappings (SDL_GameControllerDB). Don't bind host `/dev/input` into the
    session.
  - Android: needs virtual evdev, so InputReader's JoystickInputMapper creates
    `SOURCE_GAMEPAD|SOURCE_JOYSTICK` devices. Feed it a normalized Xbox-style
    layout and a vendor/product the framework knows (Generic.kl). Injecting with
    device id −1 fails: Unity, SDL and libGDX enumerate `InputDevice` ids.
  - Mac.apk: `runtime/src/gamecontroller_bridge.mm` (no rumble).
  - Test: SuperTuxKart, RetroArch, PPSSPP, Mindustry.
- [ ] **Rumble** · todo · P2 · input + bridge
  - Linux: `EVIOCSFF` and `EV_FF` writes on the host device, through the bridge.
  - Android: VibratorInputMapper (`InputDevice.getVibratorManager`). The
    supervisor turns `EVIOCSFF` on the virtual node into a bridge message.
  - Test: RetroArch (rumble option).
- [ ] **Keyboard-to-touch and D-pad mappings** · todo · P2 · sysapp
  - Mac.apk has these per-app modes: Arrow Keys as D-pad, Space Bar Taps Center,
    and key-binding profiles.
  - Android: the system app rewrites the injected stream per package (the
    focused task's package, from the window manager).
  - Test: Among Us-style joystick games; Simple Solitaire for tap-center.

### Pointer capture and relative motion

- [ ] **Pointer capture → locked pointer** · todo · P1 · input + bridge
  - Linux: `zwp_pointer_constraints_v1.lock_pointer`, plus
    `zwp_relative_pointer_v1` for unaccelerated deltas.
  - Android: `View.requestPointerCapture` reaches InputDispatcher, which hands
    the focused window `SOURCE_MOUSE_RELATIVE` events. We inject those while
    capture is on. The missing piece is a signal that capture changed. Options:
    - poll `dumpsys input` (PointerCaptureRequest) on focus changes;
    - with a virtual mouse, the cursor layer disappearing;
    - a small AOSP patch, as a last resort.

    Android 17's `POINTER_CAPTURE_MODE_ABSOLUTE` keeps absolute coordinates.

  - Test: AVNC (capture pointer), Luanti (mouse look, verify).
- [ ] **Pointer confinement** · todo · P2 · input + bridge
  - Linux: `zwp_pointer_constraints_v1.confine_pointer`.
  - Android: no public API. Only games that capture need it.

### Cursor

- [ ] **Host cursor shape from Android PointerIcon** · todo · P0 · bridge +
      input
  - Now: the cursor over the window is undefined (no `set_cursor` on enter).
  - Linux: `wp_cursor_shape_v1` for standard shapes, which follows the host
    theme. A `wl_pointer.set_cursor` surface for app bitmaps.
  - Android, two routes:
    1. Interim, immediately: set the default arrow on `enter`.
    2. Real fix: with a virtual mouse device, PointerChoreographer draws the
       icon as a SurfaceFlinger cursor layer (`Composition::CURSOR`). The
       composer turns that layer into the Wayland cursor surface and hotspot, as
       real HWCs use a cursor plane and as Waydroid's HWC does. That shows
       custom bitmaps. It can't produce `cursor-shape` names, so the cursor uses
       Android's theme, not the host's.

    The alternative is to forward `PointerIcon` types directly
    (`IInputManager.setPointerIcon` is only called by apps' ViewRootImpl), but
    that needs an AOSP patch, so it's a later option.

  - Test: Markor (I-beam over text), Fennec (hand over links).
- [ ] **Hidden cursor** · todo · P1 · input
  - Android: `PointerIcon.TYPE_NULL` (games). This falls out of the cursor-layer
    route. With cursor-shape alone, map it to a null cursor surface.

### Text input

- [x] **IME: host input methods through `text-input-v3`** · done · P0 · sysapp +
      bridge
  - Now: done (integb). "Linux.apk Keyboard" (`android/app/.../ime/`) is the
    default IME and shows nothing; the bridge's display service drives
    `zwp_text_input_v3` (`bridge/src/display/ime.rs`, `wl/text_input.rs`).
    Proven with IBus Pinyin and dead keys in nested KWin
    ([input.md](input.md#text-input)). Left: a preedit cursor inside the text
    shows at its end.
  - Linux: `zwp_text_input_v3` (enable, `set_surrounding_text`,
    `set_content_type`, `set_cursor_rectangle`, `preedit_string`,
    `commit_string`, `delete_surrounding_text`). fcitx5 and IBus serve it on KDE
    and GNOME.
  - Android: "Linux.apk Keyboard", an `InputMethodService` in the system app,
    set as the default IME (`Settings.Secure.DEFAULT_INPUT_METHOD`, via overlay
    or set at first boot). It shows no input view.
    - `onStartInput`/`EditorInfo` gives enable and content type (inputType to
      purpose/hints).
    - `onUpdateSelection` and `onUpdateCursorAnchorInfo` give the surrounding
      text and cursor rectangle (in window coordinates for that display).
    - Preedit goes to `InputConnection.setComposingText`, commit to
      `commitText`, delete to `deleteSurroundingText`.
    - Keys the host IME doesn't consume still arrive as `wl_keyboard` keys.
  - Test: Markor with fcitx5 Pinyin or Mozc; Thunderbird compose.
- [ ] **Android IME apps alongside the host IME** · todo · P2 · sysapp
  - See "App and window types": FlorisBoard and HeliBoard.
- [ ] **Spell checking** · todo · P2 · sysapp + bridge
  - Linux: Hunspell or Enchant dictionaries on the host. Nucleus `spellcheck`
    dlopens libhunspell.
  - Android: a `SpellCheckerService` in the system app, backed by the bridge.
    The image's AOSP spell checker (LatinIME dictionaries) works meanwhile.
  - Test: Markor (underlines).

### Shortcuts and navigation

- [x] **Back: Esc, the mouse back button and the Back key** · done · P0 · input
  - Now: Esc stays `KEYCODE_ESCAPE` (Android 17 desktop behaviour);
    `persist.linuxapk.input.escape_back` switches it to Back. The mouse side
    buttons send BACK/FORWARD.
- [ ] **Back button in the title bar** · todo · P0 · windowing + bridge
  - Linux: with CSD, the bridge draws a ‹ button. With SSD, there's no standard
    way to add a button. Use Alt+Left, a header bar, or the KDE app menu.
  - Android: inject `KEYCODE_BACK` on that window's display. On the root
    activity, Back moves the task to the back (Android 12+); map that to
    minimize, as Mac.apk hides the window.
- [ ] **Desktop shortcuts** · todo · P1 · input + windowing
  - Ctrl+W/Ctrl+Q close, F11 fullscreen, Ctrl+R rotate (Mac.apk ⌘R), Alt+Left
    back.
  - Linux: the bridge intercepts these before forwarding. Make them
    configurable. Super combinations belong to the compositor.
  - Android: Android 17's `KeyGestureController` already handles Meta shortcuts.
    Don't send Meta chords it would act on globally.
- [ ] **Global shortcuts for apps** · todo · P2 · bridge
  - Linux: `org.freedesktop.portal.GlobalShortcuts` (Nucleus `global-hotkey`).
  - Android: no app API. Use it only for our own actions, such as "show the last
    app".

## Windowing

- [ ] **Per-app windows** · partial · P0 · windowing
  - Now: on `lane/win` (in progress), one trusted virtual display per task, each
    shown as its own `xdg_toplevel`
    (`android/app/.../wm/WindowManagerFeature.kt`, HWC virtual displays in
    `hal/composer/ComposerClient.cpp`). On `main`, everything is display 0's
    single window.
  - Linux: `xdg_toplevel` per display.
  - Android: `DisplayManager.createVirtualDisplay` (trusted, own focus) with
    `ActivityOptions.setLaunchDisplayId`, and a `TaskStackListener`.
- [ ] **Decorations (SSD/CSD)** · partial · P0 · bridge
  - Now: SSD through `zxdg_decoration_manager_v1` (KWin).
  - Linux: xdg-decoration where offered. GNOME doesn't offer it, so the bridge
    needs CSD (libdecor, or its own header bar with back and title).
    `org_kde_kwin_server_decoration` covers older KWin (see Nucleus
    `server_decoration.rs`).
  - Android: the caption color comes from `TaskDescription` primary/status-bar
    colors (`ActivityManager.RunningTaskInfo.taskDescription`).
- [ ] **Resize** · partial · P0 · windowing
  - Now: on `lane/win`, a toplevel `configure` resizes the virtual display
    (`VirtualDisplay.resize`) to the logical size times the scale.
  - Linux: `xdg_toplevel.configure`, `set_min_size` and `set_max_size`.
  - Android: the app gets a configuration change, or is relaunched. Size compat:
    apps with `resizeableActivity=false` are letterboxed by WM. Android 16+
    ignores orientation and resizability for SDK 36+ apps on displays of 600dp
    or more.
- [ ] **Default size from orientation and `<layout>`** · todo · P0 · windowing
  - Linux: the initial toplevel size, and `set_min_size`.
  - Android: from `ActivityInfo` (`screenOrientation` and `windowLayout`:
    `defaultWidth/Height`, `minWidth/Height`, `gravity`). Portrait-locked apps
    open in a phone-shaped window, landscape in 16:9; free apps a fraction of
    the output's work area. Android 17's `AppWindowLayoutSettingsService`
    (`server/appwindowlayout/`) stores per-app user overrides; check whether it
    can persist our sizes. Mac.apk does this in `runtime/src/window_policy.cpp`
    (AOSP letterbox arithmetic) and `window_traits.cpp` (manifest parsing).
  - Test: Simple Solitaire (portrait), SuperTuxKart (landscape).
- [ ] **Dialogs and popups** · partial · P0 · windowing
  - Android: dialogs, popups and menus are windows inside the activity's
    display, so they're drawn inside the app's window. That's correct, but they
    can't extend past the window edge.
  - Linux: an `xdg_popup` per Android popup window would need per-layer
    composition (backlog: per-layer subsurfaces). P2.
- [ ] **Fullscreen** · todo · P1 · windowing
  - Linux: `xdg_toplevel.set_fullscreen`, and the `fullscreen` configure state.
  - Android: `Activity.requestFullscreenMode` (freeform API), immersive apps
    (`WindowInsetsController.hide(systemBars())`) and F11.
- [ ] **Immersive mode** · partial · P1 · windowing
  - Now: virtual displays have no system bars, so insets are zero and immersive
    apps look right.
  - Option: map an immersive request to fullscreen, per app. Games want it,
    reading apps don't.
- [ ] **Rotation** · todo · P1 · windowing
  - Android: orientation follows the display's aspect. A "Rotate" action swaps
    the window size (Mac.apk ⌘R). An app calling `setRequestedOrientation` asks
    the window to change shape (resize request).
  - Linux: `xdg_toplevel` size, which the compositor may refuse.
- [ ] **Multi-monitor** · todo · P1 · windowing + bridge
  - Linux: `wl_output` and `wp_fractional_scale_v1.preferred_scale` per surface.
    The window moves between outputs.
  - Android: per-display density and refresh rate follow the window's output. No
    Android display per monitor is needed.
- [x] **HiDPI, fractional scaling and density** · done · P0 · windowing + bridge
  - Now: each window's display is its logical size times its fractional scale
    (or the integer buffer scale without fractional-scale-v1), at 160 dpi times
    the scale. A live desktop scale change resizes the display and the app gets
    a configuration change (proof in [windowing.md](windowing.md)). Left: the
    user density multiplier below.
  - Linux: `wp_fractional_scale_v1`, `wp_viewporter`.
  - Android: `VirtualDisplay.resize(w, h, dpi)`. Add a user density multiplier:
    desktop users expect denser UI than phones (ChromeOS uses about 0.8–1.0).
    Also font scale, see System UI.
- [ ] **Refresh rate and frame pacing** · partial · P1 · bridge + hal
  - Now: a timer vsync on display 0; `wp_presentation` is only counted.
  - Linux: `wl_surface.frame` callbacks, `wp_presentation` feedback, the output
    mode refresh, `wp_commit_timing_v1`/`wp_fifo_v1`, and
    `wp_tearing_control_v1` for games.
  - Android: the HWC vsync per display,
    `VirtualDisplayConfig.setRequestedRefreshRate`, and stop vsync for hidden
    windows.
- [ ] **Focus and activation** · partial · P0 · windowing + bridge
  - Now: on `lane/win`, `keyboard_enter` focuses the display's top task.
  - Linux: `xdg_activation_v1`. Pass `XDG_ACTIVATION_TOKEN` from the `.desktop`
    launch through `linuxapk run` to the bridge. An app starting another app
    requests a token for the new window (Nucleus `activation.rs`).
  - Android: the top-resumed activity follows focus, as in multi-resume.
- [ ] **Taskbar grouping by app id** · todo · P0 · bridge + cli
  - Now: the CLI already writes `StartupWMClass=dev.linuxapk.<pkg>`, but every
    window is app id `linuxapk`.
  - Linux: `xdg_toplevel.set_app_id("dev.linuxapk.<pkg>")`, matching the
    desktop-file ID.
  - Android: the package of the display's root task.
- [ ] **Window title and icon** · todo · P1 · bridge
  - Linux: `xdg_toplevel.set_title`; `xdg_toplevel_icon_v1` (staging, KWin) for
    per-window icons. The app-id icon is the fallback.
  - Android: `RunningTaskInfo.taskDescription` (label, icon) or the activity
    label, through the `TaskStackListener` `onTaskDescriptionChanged`.
- [ ] **Minimize and restore lifecycle (onPause/onStop)** · todo · P0 ·
      windowing
  - Linux: xdg-shell has no "minimized" event. Use the `suspended` configure
    state (xdg-shell v6) and the absence of frame callbacks as the signal.
    `set_minimized` is the request.
  - Android: a suspended window turns its virtual display off or detaches its
    surface, so activities stop (`onStop`) and stop rendering. Focus loss alone
    only moves top-resumed (`onTopResumedActivityChanged`). Close finishes the
    tasks (done on `lane/win`).
- [ ] **Splash screens and launch feedback** · todo · P1 · windowing + bridge
  - Linux: map the toplevel at once, with a background color and icon from the
    app's theme, and complete the startup notification and activation token.
  - Android: the Android 12 `SplashScreen` starting window is drawn by WM Shell
    (SystemUI). Check that it shows on per-app displays; if not, the bridge
    draws a splash from `windowSplashScreenBackground` and the icon until the
    first frame.
  - Test: Read You, Tomato.
- [ ] **Window state persistence** · todo · P2 · bridge
  - Linux: Wayland clients can't position themselves. Remember the size per
    package; KDE window rules cover position.

## App and window types

- [ ] **Picture-in-picture** · todo · P2 · windowing + bridge
  - Android: `enterPictureInPictureMode` moves the task to the pinned windowing
    mode. In per-app mode the window manager resizes that task's display to the
    PiP aspect ratio (`PictureInPictureParams`) and wires `RemoteAction`s.
  - Linux: a separate small `xdg_toplevel`. Wayland has no portable "always on
    top" for clients:
    - KDE: a window rule (keep above) keyed on app id and title, or a "PiP"
      title convention;
    - KWin scripting: `setKeepAbove` through `org.kde.KWin`;
    - otherwise a plain small window, and the user can pin it.

    Track the `xx-pip`/`xdg-toplevel` PiP proposals.

  - Test: VLC (video PiP).
- [ ] **Multiple tasks/windows per app** · todo · P1 · windowing
  - Android: `FLAG_ACTIVITY_MULTIPLE_TASK`, `FLAG_ACTIVITY_NEW_DOCUMENT`,
    `documentLaunchMode`, browsers' "new window". Each new task gets its own
    display and window.
  - Linux: all of an app's windows share `app_id dev.linuxapk.<pkg>`, so the
    taskbar groups them. A `.desktop` "New Window" action launches with
    `MULTIPLE_TASK` if the launcher activity allows it.
  - Test: Markor (documents, verify), Fennec (new window, verify).
- [ ] **Translucent and dialog-themed activities** · todo · P1 · windowing +
      bridge
  - Android: `windowIsTranslucent` and `Theme.*.Dialog` activities in their own
    task, such as share targets or PackageInstaller's confirmation. With the
    previous activity in the same task, they render inside its window.
  - Linux: when a translucent or dialog activity is alone in its task, give the
    window an alpha buffer (`ARGB8888` with a real alpha channel, not
    `XBGR8888`), size it to the dialog, and parent it to the caller's window
    (`xdg_toplevel.set_parent`, or `xdg_dialog_v1` for modal hints). Otherwise
    show a dimmed backdrop.
  - Test: PackageInstaller dialog from F-Droid; KDE Connect's share target.
- [ ] **`SYSTEM_ALERT_WINDOW` overlays (chat heads, floating widgets)** · todo ·
      P2 · windowing
  - Android: `TYPE_APPLICATION_OVERLAY` windows go on the display of the app's
    context. In per-app mode that's display 0, which has no window.
  - Linux: options:
    - give display 0 a window while an overlay exists;
    - per overlay window, a borderless always-on-top toplevel (same caveats as
      PiP); `wlr-layer-shell` is not on GNOME;
    - won't-do for less common uses.
  - Test: NewPipe (popup player).
- [ ] **Cross-app task stacks** · todo · P0 · windowing
  - Android: app A starts app B's activity in A's task (the default without
    `NEW_TASK`), for example a share target or a picker. It stays in A's window,
    which matches phone behaviour. `NEW_TASK` starts get their own window.
    `TaskStackListener` maps new tasks to displays.
  - Linux: a new window is activated with a token requested from A's surface.
  - Test: Thunderbird opening a link or attachment; F-Droid to the installer.
- [ ] **Presentation API and second-screen apps** · todo · P2 · windowing
  - Android: `DisplayManager` shows public presentation displays; `Presentation`
    windows go there.
  - Linux: a user action ("Add second screen") creates a public virtual display
    shown as another toplevel the user can put on a projector output.
  - Test: VLC (presentation on secondary display).
- [ ] **Immersive/fullscreen and orientation-locked games** · todo · P1 ·
      windowing
  - Android: fixed `screenOrientation`, immersive sticky, and
    `FLAG_KEEP_SCREEN_ON`.
  - Linux: the window opens in the locked orientation's shape, with a fullscreen
    toggle (F11). It keeps aspect when resized: `xdg_toplevel` has no aspect
    hint, so the bridge letterboxes inside the buffer. Keep-awake is under
    System services.
  - Test: SuperTuxKart, Mindustry, Shattered Pixel Dungeon.
- [ ] **Bubbles** · won't-do · P2 · windowing
  - Android: `Notification.BubbleMetadata`. SystemUI shows bubbles over other
    apps.
  - Reason: few apps use them on desktop. Notifications carry the content, and
    the expanded bubble is just the activity; offer it as a normal window from
    the notification action.
  - Test: no FOSS app known to use bubbles; a test APK.
- [ ] **IME apps (FlorisBoard, HeliBoard)** · todo · P2 · sysapp
  - Android: the user may pick an Android IME in Settings. If they do, our
    `text-input-v3` IME isn't active. The IME window shows inside the app's
    display (as on a tablet), and `wl_keyboard` keys still go through.
  - Linux: our IME is the default. Expose the IME choice in the manager app.
  - Test: FlorisBoard, HeliBoard.
- [ ] **Accessibility-service apps** · todo · P2 · sysapp
  - Android: third-party `AccessibilityService`s (Key Mapper, automation) work
    inside the session. They see Android windows only, and global gestures
    target display 0.
  - Linux: none. They coexist with our own AT-SPI service (see Accessibility).
  - Test: Key Mapper.
- [ ] **Notification-listener apps** · partial · P2 · sysapp
  - Android: `NotificationListenerService` apps work as on Android once the user
    grants access in Settings. The system app's own listener is one of them.
  - Test: KDE Connect (notification sync), Gadgetbridge.
- [x] **VPN apps (`VpnService`)** · done · P1 · session
  - Now: the session binds `/dev/net/tun` (`session/ns-init.sh`). The tunnel
    lives in the session's network namespace, so it only affects Android apps,
    not the host.
  - Linux: nothing to integrate. A host VPN already covers the session through
    pasta.
  - Test: WireGuard, NetGuard, RethinkDNS.
- [ ] **Headless foreground services (music after the window closes)** · todo ·
      P0 · session + bridge + sysapp
  - Android: a foreground service with a notification (`mediaPlayback`) keeps
    playing after the activity's window closes.
  - Linux: the session must stay alive while any foreground service runs. Idle
    shutdown counts FGS, not just windows. MPRIS stays published, and the FGS
    notification becomes a persistent host notification. Clicking it reopens the
    window. Optionally, a StatusNotifierItem tray icon lists running services
    (see Background execution).
  - Test: AntennaPod, VLC, Auxio.
- [ ] **Launcher/home replacements** · won't-do · P2 · —
  - Reason: the desktop is the launcher. `HOME` intents stay on display 0, which
    is hidden. Apps that need to be the default launcher have no role here.
  - Test: KISS launcher (should install and not break anything).
- [ ] **Live wallpapers** · won't-do · P2 · —
  - Reason: there's no Android wallpaper surface on the desktop.
    `WallpaperService` apps install but have nothing to draw on. Wallpaper
    colors for theming are under System UI.
- [ ] **App widgets** · todo · P2 · sysapp + bridge
  - Android: `AppWidgetHost` in the system app, binding providers with
    `BIND_APPWIDGET`, and rendering `RemoteViews`.
  - Linux: a "widgets" window (one toplevel per widget, or a small panel app)
    showing the rendered views. No desktop has a portable widget host (Plasma
    widgets and GNOME extensions differ). Start with a widget board window in
    the manager app.
  - Test: Fossify Clock and Tasks.org widgets.
- [ ] **Android TV/leanback apps** · todo · P2 · input + windowing
  - Android: `LEANBACK_LAUNCHER`-only apps. The CLI's resolver must fall back to
    it. D-pad navigation from the arrow keys, Enter and Esc (`KEYCODE_DPAD_*`,
    `KEYCODE_DPAD_CENTER`). Mac.apk does the same.
  - Linux: the window opens at 16:9.
  - Test: Kodi.
- [ ] **App shortcuts as `.desktop` actions** · todo · P1 · cli + sysapp
  - Android: static shortcuts from `res/xml/shortcuts.xml` (the manifest's
    `android.app.shortcuts` meta-data), read with aapt2 at install time. Dynamic
    and pinned shortcuts come from `LauncherApps.getShortcuts` (the system app
    holds `ACCESS_SHORTCUTS` as a launcher-like app) and change at runtime.
  - Linux: `.desktop` `Actions=` with `Exec=linuxapk run --shortcut <id> <pkg>`.
    Update the file when dynamic shortcuts change. Also Unity LauncherEntry
    quicklists (Nucleus `launcher-linux`, dbusmenu).
  - Android launch: `LauncherApps.startShortcut`.
  - Test: Tasks.org, Fossify Notes (verify).
- [ ] **OAuth and Custom Tabs sign-in flows** · partial · P0 · sysapp + cli +
      bridge
  - Now: the CLI registers custom-scheme handlers
    (`MimeType=x-scheme-handler/<scheme>`), so a host browser redirect to
    `com.example.app:/oauth2redirect` reaches the app as a `VIEW` intent.
  - Android: there's no Android browser in the image. The system app is the
    default browser and the Custom Tabs provider:
    - handle `VIEW` http(s) with `BROWSABLE`;
    - provide a `CustomTabsService`
      (`android.support.customtabs.action.CustomTabsService`), so AppAuth and
      Custom Tabs see a browser;
    - forward the URL to the bridge, which calls `OpenURI.OpenURI` (the host
      browser). The redirect returns through the scheme handler. That covers
      AppAuth with custom schemes.
  - Gaps:
    - **https App Link redirects** (verified `assetlinks.json` hosts, already
      recorded as `X-LinuxApk-Web-Hosts`): the host browser just loads the page.
      Fix with a sign-in window, a WebView activity in the system app that
      intercepts navigation to the app's verified link and hands it to the app.
      This is Mac.apk's `runtime/src/applink_window.mm`. Or with an optional
      desktop URL router (`.desktop` for `x-scheme-handler/https` that forwards
      known hosts).
    - Google blocks its sign-in in WebViews, so Google accounts must use the
      host browser (see Accounts).
  - Test: Thunderbird (Gmail/Outlook OAuth), Tusky (Mastodon OAuth).
- [x] **Deep links from the desktop** · done · P0 · cli
  - Now: `linuxapk run <pkg> <uri>` sends `VIEW`+`BROWSABLE`;
    `xdg-open wikipedia://…` works (`docs/cli.md`). http(s) is never claimed.
  - Next: a URL router for verified web hosts (see OAuth), and `geo:`, `mailto:`
    and `tel:` scheme handlers when an app declares them.
  - Test: Wikipedia; Organic Maps (`geo:`).

## System UI and appearance

- [ ] **Dark mode** · todo · P0 · sysapp + bridge
  - Linux: `org.freedesktop.portal.Settings`, `org.freedesktop.appearance`
    `color-scheme` (0 default, 1 dark, 2 light), with `SettingChanged`. Nucleus
    `darkmode-detector`.
  - Android: `UiModeManager.setNightModeActivateForCustomMode` or
    `setNightMode(MODE_NIGHT_YES/NO)` (`MODIFY_DAY_NIGHT_MODE`), live. Mac.apk
    does this live.
  - Test: Read You, Tomato.
- [ ] **Accent color and Material You** · todo · P1 · sysapp + bridge
  - Linux: `org.freedesktop.appearance` `accent-color` (sRGB doubles; KDE and
    GNOME 47+). Nucleus `system-color`.
  - Android: Android 17's `IThemeManager.updateThemeSettings` (new in 17:
    `frameworks/base/core/java/android/content/theming/`, served by
    `server/theming/ThemeBinderService.java`) with `ThemeSettings.setSeedColors`
    and the preset color source. On older paths,
    `Settings.Secure.THEME_CUSTOMIZATION_OVERLAY_PACKAGES` JSON
    (`system_palette`, `color_source: preset`), which SystemUI's
    ThemeOverlayController applies.
  - Fallback: with no accent set, set the host wallpaper as the Android
    wallpaper so Android extracts the seed (Mac.apk
    `runtime/src/wallpaper_bridge.mm`). Read it from the Wallpaper portal or a
    file.
  - Test: Read You, Auxio (dynamic color).
- [ ] **Font scaling** · todo · P1 · sysapp + bridge
  - Linux: the portal Settings `org.gnome.desktop.interface`
    `text-scaling-factor`. KDE's font DPI is in `kdeglobals` (`forceFontDPI`).
  - Android: `Settings.System.FONT_SCALE` (nonlinear scaling from Android 14).
- [ ] **Fonts** · partial · P2 · image
  - Now: the image's Roboto and Noto, including CJK and emoji.
  - Linux: the host UI font through fontconfig. Mapping it onto Android isn't
    worth it, since apps bundle fonts.
  - Mac.apk backs CJK families with host faces (`runtime/src/host_fonts.cpp`).
    Only needed if we trim Noto CJK from the image.
- [ ] **Locale and regional preferences** · todo · P0 · sysapp
  - Linux: `LANG`/`LC_*` from the session environment;
    `org.freedesktop.locale1`; the Account portal for the user name.
  - Android: `LocalePicker.updateLocales` or
    `ActivityManager.updatePersistentConfiguration` (`CHANGE_CONFIGURATION`) at
    startup and on change. `LC_TIME`/`LC_MEASUREMENT` become Unicode extensions
    (`-u-fw-`, `-mu-`) for Android 14 regional preferences. Per-app languages
    (`LocaleManager`) already work inside Android.
- [ ] **Time zone** · todo · P0 · sysapp + bridge
  - Now: the session uses the kernel clock, so the time is right, but the zone
    is the image default.
  - Linux: `org.freedesktop.timedate1` `Timezone` (PropertiesChanged), or
    `/etc/localtime`.
  - Android: `AlarmManager.setTimeZone` (`SET_TIME_ZONE`). Turn off automatic
    time-zone detection (`time_zone_detector`) so telephony and location sources
    don't fight it.
  - Test: Fossify Clock, Etar.
- [ ] **12/24-hour time format** · todo · P1 · sysapp
  - Linux: GNOME `clock-format` through the portal Settings; KDE through
    `LC_TIME`.
  - Android: `Settings.System.TIME_12_24`.
- [ ] **Reduced motion** · todo · P1 · sysapp + bridge
  - Linux: `org.freedesktop.appearance` `reduced-motion` (xdg-desktop-portal
    1.20+).
  - Android: `Settings.Global` `animator_duration_scale`,
    `transition_animation_scale` and `window_animation_scale` set to 0 (the
    "Remove animations" switch).
- [ ] **High contrast** · todo · P2 · sysapp + bridge
  - Linux: `org.freedesktop.appearance` `contrast` (1 = high).
  - Android: `Settings.Secure.CONTRAST_LEVEL` (UiModeManager contrast, Android
    14+), and optionally `high_text_contrast_enabled`.
- [ ] **Notifications** · todo · P0 · sysapp + bridge
  - Linux: `org.freedesktop.Notifications` (Nucleus `notification-linux`) or
    `org.freedesktop.portal.Notification` v2 (needed inside Flatpak).
    `desktop-entry` hint `dev.linuxapk.<pkg>` gives the app's name and icon.
    Waydroid has the same split (`tools/services/notification_manager.py`).
  - Android: a `NotificationListenerService` in the system app. Grant access
    through the `config_defaultListenerAccessPackages` overlay or
    `cmd notification allow_listener` at setup.
    - Post and remove: `onNotificationPosted/Removed`, with the `replaces_id`
      map keyed by the notification key.
    - Title, text, big text, large icon and images: `image-data` hint.
    - Clicks: the `contentIntent` `PendingIntent.send()` opens the app's window,
      with an activation token from the `ActivationToken` signal.
  - Test: Tasks.org reminders, Thunderbird.
- [ ] **Notification actions** · todo · P0 · sysapp + bridge
  - Linux: the `actions` list and `ActionInvoked`.
  - Android: send `Notification.Action.actionIntent`.
- [ ] **Inline reply** · todo · P1 · sysapp + bridge
  - Linux: KDE's `inline-reply` capability (`NotificationReplied`), and portal
    Notification v2's text-input action where supported.
  - Android: `RemoteInput.addResultsToIntent` and send.
  - Test: Conversations.
- [ ] **Progress** · todo · P1 · sysapp + bridge
  - Linux: the `value` hint (KDE, GNOME ignores it), and Unity `LauncherEntry`
    `progress` on the app's taskbar entry (Nucleus `taskbar-progress`).
  - Android: `EXTRA_PROGRESS`/`EXTRA_PROGRESS_MAX`, and Android 16
    `ProgressStyle`.
  - Test: F-Droid (download progress), NewPipe downloads.
- [ ] **Channels, importance, DND** · todo · P1 · sysapp + bridge
  - Map importance to urgency and category; silent channels to `suppress-sound`.
    Host Do Not Disturb (`Inhibited` on KDE, the GNOME `show-banners` setting)
    maps to Android Zen through `NotificationManager.setInterruptionFilter`, or
    simply suppresses at the bridge. Channel settings stay in Android Settings.
- [ ] **Badges** · todo · P2 · sysapp + bridge
  - Linux: Unity `LauncherEntry` `count` (KDE and Dash to Dock).
  - Android: `Notification.number` or the count of active notifications per
    package.
  - Test: Thunderbird unread count.
- [ ] **Toasts** · todo · P1 · windowing
  - Android: text toasts (targetSdk 30+) are drawn by SystemUI's ToastUI on the
    app's display. Check that SystemUI draws them on per-app virtual displays
    (it may need `VIRTUAL_DISPLAY_FLAG_SHOULD_SHOW_SYSTEM_DECORATIONS`). If it
    doesn't, map them to transient host notifications. Custom-view toasts are
    app windows and already render.
- [ ] **Share sheet** · todo · P1 · sysapp + bridge
  - Linux: there's no share portal. The Android chooser (IntentResolver) already
    works in the app's window and lists Android targets.
  - Android: add system-app targets to the chooser (an exported `ACTION_SEND`
    activity):
    - "Open with a desktop app": the bridge calls `OpenURI.OpenFile` with
      `ask: true` and a passed fd;
    - "Save to file…": `FileChooser.SaveFile`;
    - "Copy";
    - "Send by email": the Email portal.
  - Test: Markor, Fossify Gallery, KDE Connect.
- [ ] **Status bar, quick settings and recents** · won't-do · — · —
  - Reason: the desktop shell provides these, and display 0's SystemUI is never
    shown in per-app mode.

## Files and content

- [ ] **SAF picker to the FileChooser portal** · todo · P0 · sysapp + bridge
  - Linux: `org.freedesktop.portal.FileChooser` `OpenFile` and `SaveFile`, with
    filters from MIME types. Parent window through `xdg_foreign`
    (`zxdg_exporter_v2`), as Nucleus `share` does. Mac.apk:
    `runtime/src/document_picker.mm`.
  - Android: a system-app activity that handles `ACTION_OPEN_DOCUMENT`,
    `ACTION_GET_CONTENT` and `ACTION_CREATE_DOCUMENT` with an intent-filter
    priority above DocumentsUI's (100). Priorities above 0 are honored for priv
    apps. It returns `content://dev.linuxapk.host/…` URIs from our
    DocumentsProvider (next row), with grant flags. Keep DocumentsUI reachable
    as a setting.
  - Test: Markor (open/save), AnkiDroid import.
- [ ] **`ACTION_OPEN_DOCUMENT_TREE`** · todo · P1 · sysapp + bridge
  - Linux: FileChooser with `directory: true`.
  - Android: a tree URI on our provider rooted at the chosen folder.
    `takePersistableUriPermission` works on our provider. The bridge persists
    the host path through the Documents portal or its own grant store.
  - Test: Markor (notebook folder), Syncthing-Fork.
- [ ] **DocumentsProvider over host folders** · todo · P1 · sysapp + bridge
  - Android: `HostDocumentsProvider` in the system app (`MANAGE_DOCUMENTS`):
    `queryRoots`, `queryChildDocuments` and `openDocument`. The bridge answers
    with listings and opened fds (`ParcelFileDescriptor` from the passed fd),
    never paths. Roots appear in DocumentsUI too. Mac.apk:
    `runtime/java/com/macapk/loader/HostDocumentsProvider.java`.
  - Linux: only folders the user granted through the portal, plus optional XDG
    user dirs.
- [ ] **Shared storage and XDG user folders** · todo · P1 · session
  - Now: `/sdcard` is private session storage under
    `~/.local/share/linuxapk/data*`.
  - Option: opt-in mapping of `~/Downloads` and `~/Pictures` into
    `/sdcard/Download` and `Pictures`, as Mac.apk does with real Mac folders.
    This is a deliberate exception to "no `$HOME` binds": named dirs only, under
    MediaProvider's FUSE. The safer default is the DocumentsProvider.
- [ ] **MediaStore and the photo picker** · todo · P1 · sysapp + session
  - Android: the photo picker (`ACTION_PICK_IMAGES`, MediaProvider module) shows
    MediaStore. It's useful only with mapped folders, or with a
    `CloudMediaProvider` in the system app backed by the host Pictures folder
    (an Android 13+ API).
  - Linux: FileChooser with image filters is the simplest fallback. Mac.apk
    reads the Photos library (`runtime/src/photos_bridge.mm`).
  - Test: Fossify Gallery, Telegram-FOSS (picker).
- [ ] **Downloads** · todo · P2 · sysapp + bridge
  - Android: `DownloadManager` completes into `/sdcard/Download`.
  - Linux: a "Show in folder" notification action
    (`org.freedesktop.FileManager1` `ShowItems`), or a mapped `~/Downloads`.
- [ ] **Drag and drop: desktop into Android** · todo · P2 · bridge + sysapp
  - Linux: `wl_data_device` on the app's surface (`text/uri-list`, `text/plain`,
    images), via the FileTransfer portal when sandboxed. Nucleus `dnd.rs`.
  - Android: the system app puts a transparent window on the target display and
    starts `View.startDragAndDrop` there, with a `ClipData` of
    `content://dev.linuxapk.host` URIs and `DRAG_FLAG_GLOBAL_URI_READ`. Injected
    motion then carries it to the drop point. Investigate whether an injected
    `ACTION_DRAG_*` path exists first.
  - Test: Markor (drop text), Fossify Gallery (drop images, verify).
- [ ] **Drag and drop: Android out to the desktop** · todo · P2 · bridge +
      sysapp
  - Android: `IWindowManager.setGlobalDragListener` (`IGlobalDragListener`
    `onUnhandledDrop`, Android 15+) tells us when a global drag isn't handled.
    The drag must have started with `DRAG_FLAG_GLOBAL`.
  - Linux: when the pointer leaves the window mid-drag, the bridge starts
    `wl_data_device.start_drag`, offering the URIs as files (exported through
    the bridge).
- [ ] **Clipboard: text** · todo · P0 · sysapp + bridge
  - Linux: `ext_data_control_v1` (or `zwlr_data_control_v1`) for focus-less sync
    on KDE and wlroots. On GNOME, `wl_data_device` selection while one of our
    windows has focus.
  - Android: `ClipboardManager.addPrimaryClipChangedListener` and
    `setPrimaryClip` in the system app (`READ_CLIPBOARD_IN_BACKGROUND`,
    privileged). Respect `ClipDescription.EXTRA_IS_SENSITIVE`: pass
    `x-kde-passwordManagerHint` and don't keep it in history. Waydroid:
    `hwcomposer/WaydroidClipboard.cpp`. Mac.apk:
    `runtime/src/clipboard_bridge.mm`.
  - Test: Markor, KeePassDX (sensitive).
- [ ] **Clipboard: images and URIs** · todo · P1 · sysapp + bridge
  - Out: read the `content://` stream and offer `image/png` or the file.
  - In: host image data goes behind a system-app content URI in a `ClipData`.
- [ ] **File associations** · todo · P1 · cli + sysapp
  - Now: the CLI skips intent filters with a `mimeType`.
  - Linux: `.desktop` `MimeType=` from exported `VIEW` filters with MIME types,
    and `Exec=linuxapk run <pkg> %F`. Skip `*/*`.
  - Android: the bridge passes the file's fd, and the system app serves it as
    `content://dev.linuxapk.host/…` in a `VIEW` intent with
    `FLAG_GRANT_READ_URI_PERMISSION`, restricted to the package.
  - Test: Markor (`text/markdown`), VLC (video files).
- [ ] **Opening URLs and files from Android on the desktop** · todo · P0 ·
      sysapp + bridge
  - Android: the system app handles http(s), `mailto:` and others no Android app
    claims, and files no Android app can open. It's the default browser role
    (`RoleManager` `ROLE_BROWSER`, set at setup).
  - Linux: `org.freedesktop.portal.OpenURI` `OpenURI` and `OpenFile` (an fd),
    with an activation token. Nucleus `share`.
  - Test: Wikipedia external links, Thunderbird links.

## Media

- [ ] **Audio out** · partial · P0 · audio + bridge
  - Now: on `lane/audio`, a bridge-backed `libtinyalsav2` (`docs/audio.md`)
    writes into memfd rings, and `bridge/` plays them to PipeWire (Pulse
    protocol).
  - Next:
    - one host stream per Android app (from `AudioPlaybackConfiguration` uids),
      with `application.name`, `application.icon-name` and `media.role`, so the
      mixer shows Android apps by name;
    - latency tuning.
  - Test: AntennaPod, Shattered Pixel Dungeon.
- [ ] **Audio in (microphone)** · todo · P1 · audio + bridge
  - Linux: a PipeWire capture stream. The Camera portal has no audio analog, so
    inside Flatpak this needs `--socket=pulseaudio`. Show the host mic
    indicator.
  - Android: the HAL's input stream through the same tinyalsa replacement (a
    capture PCM). Android 12+ mic toggles and the privacy indicator already
    work. Mac.apk: `runtime/src/audio_input.mm`.
  - Test: Fossify Voice Recorder.
- [ ] **MediaSession to MPRIS** · todo · P1 · sysapp + bridge
  - Linux: `org.mpris.MediaPlayer2.dev_linuxapk_<pkg>` (`Player`: metadata, art
    URL as a file, PlaybackStatus, Position, CanGoNext, Seek, `DesktopEntry`).
    Nucleus `media-control`.
  - Android: `MediaSessionManager.getActiveSessions` plus
    `addOnActiveSessionsChangedListener` (`MEDIA_CONTENT_CONTROL`, privileged).
    `MediaController.Callback` gives metadata and state; `TransportControls`
    take the commands. Media keys then work through the desktop. Mac.apk:
    `runtime/src/nowplaying_bridge.mm`.
  - Test: AntennaPod, VLC, Auxio.
- [ ] **Volume** · todo · P1 · audio + bridge
  - Linux: per-app stream volume in the PipeWire mixer.
  - Android: keep Android's own stream volumes at max by default, so the host
    mixer is the single control. Map Android's "Master Playback Volume" mixer
    control (already in `docs/audio.md`) for apps that set volume themselves.
- [ ] **Audio focus and ducking** · todo · P2 · audio
  - Android: focus is arbitrated inside Android. Duck other host players? No
    standard API exists. Leave it within Android.
- [ ] **Camera** · todo · P1 · hal + bridge
  - Linux: `org.freedesktop.portal.Camera` (`AccessCamera`,
    `OpenPipeWireRemote`) to PipeWire video nodes.
  - Android: a camera provider HAL (AIDL `ICameraProvider`, a vendor service)
    whose devices are PipeWire nodes from the bridge, with frames in shared
    gralloc buffers. AOSP's external (V4L2) camera HAL is the reference design
    but would need `/dev/video*` in the session. The emulator camera HALs are
    masked today (`session/rootfs-overlay/vendor/etc/init/*camera*.rc`). Effort
    L. Mac.apk: `runtime/src/camera_capture.mm`.
  - Test: Binary Eye (QR), Open Camera, Aegis.
- [ ] **Screen capture and MediaProjection** · todo · P2 · windowing + bridge
  - Android: MediaProjection captures display 0 by default, which is empty in
    per-app mode. Point projections at the app's own display, or
    `createVirtualDisplay` of a chosen task (Android 14 app-content sharing).
  - Linux: to capture the desktop, `org.freedesktop.portal.ScreenCast` feeds a
    virtual display surface. Desktop recorders capturing Android windows already
    work.
  - Test: a MediaProjection screen recorder from F-Droid (verify which).
- [ ] **Video decode acceleration** · todo · P1 · hal + gpu
  - Now: software Codec2 (`c2.android.*`) on the BLOB pool
    (`debug.stagefright.c2-poolmask`).
  - Linux: VA-API on `/dev/dri/renderD128` (Mesa radeonsi/iHD), built for
    Bionic.
  - Android: a Codec2 HAL service (`IComponentStore`) wrapping FFmpeg with
    VA-API, as Android-x86 and BlissOS do (`c2.ffmpeg.*`). Output goes to
    gralloc buffers; zero-copy needs VA surface to dma-buf export. Effort L.
  - Test: VLC, NewPipe.
- [ ] **DRM (Widevine)** · won't-do · — · —
  - Reason: Widevine needs Google's license and attestation. ClearKey (AOSP)
    works, and Netflix-class apps are out of scope, like Play Integrity.
- [ ] **Text-to-speech** · todo · P2 · sysapp + bridge
  - Linux: speech-dispatcher (`speechd` socket), or an engine bundled in Android
    (eSpeak NG Android, RHVoice).
  - Android: a `TextToSpeechService` in the system app that forwards to speechd.
  - Test: Librera Reader (read aloud).
- [ ] **MIDI** · todo · P2 · audio
  - Linux: ALSA sequencer or PipeWire MIDI.
  - Android: `MidiDeviceService` in the system app.

## Sensors and hardware

- [ ] **Location** · todo · P1 · sysapp + bridge
  - Linux: `org.freedesktop.portal.Location` (`CreateSession`, `Start`,
    `LocationUpdated`; GeoClue behind it), with the portal's consent dialog.
  - Android: a location provider in the system app (`LocationProviderBase`,
    `INSTALL_LOCATION_PROVIDER`), configured by overlay as the network and fused
    provider (`config_enableNetworkLocationOverlay`,
    `config_networkLocationProviderPackageName`, and the fused equivalents). The
    emulator GNSS HAL is masked. Android's own runtime permission still gates
    apps. Mac.apk: `runtime/src/location_bridge.mm`.
  - Test: Organic Maps, StreetComplete.
- [ ] **Accelerometer and tilt for games ("Tilt Control")** · todo · P2 · hal +
      input
  - Android: a Sensors Multi-HAL 2.1 sub-HAL (`ISensorsSubHal` `.so`, loaded by
    the existing multihal service) providing accelerometer, gravity, gyroscope
    and rotation vector. Values come from the bridge. The multihal service is
    masked today.
  - Linux: two sources:
    - Tilt Control, as in Mac.apk: the arrow keys or a gamepad stick tilt the
      device (roll and pitch, springing back to level), toggled per app
      (`runtime/java/com/macapk/loader/TiltControl.java`);
    - real sensors on 2-in-1s through iio-sensor-proxy
      (`net.hadess.SensorProxy`: accelerometer orientation, light).

    At rest it reports a device lying still at 1 g.

  - Test: SuperTuxKart (tilt steering), SatStat (sensor readout).
- [ ] **Vibration and haptics** · todo · P2 · hal
  - Android: the vibrator HAL is an example HAL today, so calls succeed
    silently. That's correct for desktops.
  - Linux: route to gamepad rumble when a controller is active, or feedbackd
    (`org.sigxcpu.Feedback`) on Linux phones.
- [ ] **Battery and power** · todo · P1 · hal + bridge
  - Linux: UPower `DisplayDevice` (percentage, state, time to empty) and
    power-profiles-daemon or the PowerProfileMonitor portal (power saver).
    Nucleus `system-info` and `energy-manager`.
  - Android: our own Health AIDL HAL (vendor, NDK) fed by the bridge. Host
    `/sys/class/power_supply` is masked in the session. Without a battery,
    report AC and full. Power saver maps to
    `PowerManager.setPowerSaveModeEnabled`.
  - Test: AntennaPod (pauses downloads on battery saver, verify).
- [ ] **Network state and metering** · partial · P1 · sysapp + bridge
  - Now: Ethernet on pasta's `eth1`, validated (`docs/compat-triage.md`).
  - Linux: the NetworkMonitor portal (`metered`, `connectivity`) or
    NetworkManager `Metered`.
  - Android: `EthernetManager.updateConfiguration` with capabilities that drop
    `NOT_METERED`, so apps honor metered networks.
  - Test: F-Droid and AntennaPod (metered-only settings).
- [ ] **Proxy** · todo · P2 · sysapp + bridge
  - Linux: the ProxyResolver portal, or the `http_proxy` environment.
  - Android: `ConnectivityManager.setGlobalProxy` (`Settings.Global`
    `http_proxy`).
- [ ] **Low memory** · todo · P2 · session + bridge
  - Linux: the MemoryMonitor portal (`LowMemoryWarning`), and PSI on the session
    cgroup.
  - Android: lmkd and `onTrimMemory`. Known issue: lmkd can't lower
    `oom_score_adj` (`docs/compat-triage.md`). Feed host pressure into lmkd's
    PSI thresholds, or trim cached apps.
- [ ] **Bluetooth** · won't-do · P2 · —
  - Now: disabled (empty permission XMLs in `session/rootfs-overlay`), because
    there's no HCI HAL.
  - Reason: audio devices already work through PipeWire, and BLE apps (fitness
    bands) would need a Bluetooth HAL over BlueZ D-Bus with exclusive adapter
    access. Revisit for Gadgetbridge-style demand.
- [ ] **USB host** · todo · P2 · hal + bridge
  - Linux: the `org.freedesktop.portal.Usb` portal (xdg-desktop-portal 1.19+):
    `AcquireDevices` gives fds.
  - Android: `UsbManager` host mode needs a USB HAL and usbfs-like device nodes.
    Only for niche apps (serial adapters, SDR).
- [ ] **NFC, telephony, SMS, IR, UWB, radio** · won't-do · — · —
  - Reason: no desktop hardware or meaningful mapping. The radio and Wi-Fi HALs
    are masked. Phone integration is KDE Connect's job.
- [ ] **Lights (LED notifications), consumer IR, thermal** · won't-do · — · —
  - Reason: example HALs are enough. Thermal could later map host thermals to
    `PowerManager.getCurrentThermalStatus` for games (P2).

## System services

- [ ] **Keep awake** · todo · P1 · windowing + bridge
  - Linux: `zwp_idle_inhibit_manager_v1` on the window, as Waydroid does, while
    it's visible, or the Inhibit portal (`idle`). Nucleus `energy-manager`.
  - Android: `FLAG_KEEP_SCREEN_ON` windows and `SCREEN_*` wake locks. The signal
    is per display, and needs investigation: WindowManager's hold-screen window
    per display, or `IPowerManager` wake-lock state. Also stop Android's own
    screen timeout (`Settings.System.SCREEN_OFF_TIMEOUT` maximum,
    `stay_on_while_plugged_in`).
  - Test: VLC and NewPipe (video playback).
- [ ] **Host lock and idle as screen off** · todo · P2 · sysapp + bridge
  - Linux: logind `LockedHint` and `org.freedesktop.ScreenSaver`
    `ActiveChanged`.
  - Android: `ACTION_SCREEN_OFF/ON` and pausing apps. Keep media playing.
- [ ] **Alarms while the session is stopped** · todo · P1 · session + sysapp
  - Now: `AlarmManager` works while the session runs, but idle shutdown would
    lose wakeups.
  - Linux: a systemd user timer (Nucleus `scheduler` uses transient timers).
    Before shutdown, the system app reports the next wakeup alarm
    (`AlarmManager.getNextAlarmClock` plus the exact-alarm queue), and the timer
    restarts the session.
  - Test: Fossify Clock (alarm), Tasks.org reminders.
- [ ] **JobScheduler and WorkManager with no windows open** · todo · P1 ·
      session
  - Android: jobs run while the session runs.
  - Policy: keep the session alive while any app has pending periodic work?
    That's too aggressive. Propose a per-app "run in background" setting
    (default off). Allowed apps keep the session up, with a tray indicator.
    Others are stopped with the session (Android "force-stopped" semantics, like
    Doze).
  - Test: Read You (sync), AntennaPod (refresh).
- [ ] **Background execution, autostart and boot** · todo · P2 · session +
      bridge
  - Linux: the Background portal `RequestBackground` (autostart and permission;
    Nucleus `autolaunch`), or an XDG autostart entry or systemd user unit, plus
    a StatusNotifierItem tray icon showing the session and its foreground
    services.
  - Android: `BOOT_COMPLETED` fires at each session start. Apps the user allows
    in the background get the session started at login.
  - Test: Syncthing-Fork, KDE Connect.
- [ ] **Accounts and GMS/microG** · partial · P1 · image + sysapp
  - Now (integb): `tools/microg.sh` installs microG as privileged system apps
    through the overlay; location, checkin and its push connection work, but
    apps using the Play Services libraries don't (no signature spoofing on AOSP
    17; the smallest patch is sketched). UnifiedPush through ntfy works end to
    end. See [microg.md](microg.md).
  - Android: microG (GmsCore, GsfProxy, the FakeStore companion) as priv-apps in
    our source-built image. Signature spoofing needs a documented AOSP patch
    (the image lane's patch queue), since the CI image can't do it. Gives FCM
    push (needs the session alive, see background), maps, location and
    SafetyNet-free sign-in.
  - Linux: Google sign-in must use a real browser page (Google blocks WebViews).
    Mac.apk renders Google's EmbeddedSetup flow in a window
    (`runtime/src/gms_signin.mm`); microG's own flow does the same in a WebView.
  - Mac.apk ships its own GMS subset, including licence checks.
  - Test: microG self-check; Tusky and Element (UnifiedPush works without it).
- [ ] **WebView provider and version** · partial · P0 · image
  - Now: the image's AOSP WebView works: Wikipedia's articles render
    (`COMPATIBILITY.md`).
  - Next: track Chromium updates with our image builds, and allow updatable
    providers (`config_webview_packages.xml` overlay, e.g. Vanadium or Bromite
    builds) for security fixes between image releases.
- [ ] **App stores and installs** · partial · P0 · sysapp + cli + bridge
  - Now: F-Droid and Aurora run (`COMPATIBILITY.md`), but installs from them
    haven't been tested (the M3 target in `docs/compat-triage.md`).
  - Android: PackageInstaller sessions show the confirmation dialog
    (PackageInstaller app). It must appear in the store's window (a dialog task,
    see App and window types). The unknown-sources appop works as on Android.
  - Linux: the system app listens for `PACKAGE_ADDED/REPLACED/REMOVED`
    (`LauncherApps.Callback`). The bridge then writes or removes the `.desktop`
    file and icons with the CLI's shared library (`cli/`), and shows a host
    notification. Mac.apk adds an OS-level approval (Touch ID) for every store
    install. An equivalent here would be a bridge-side confirmation dialog (P2).
  - Test: F-Droid, Droid-ify, Aurora Store, Obtainium.
- [ ] **Permissions UX mapped to portals** · todo · P1 · sysapp + bridge
  - Android: runtime permission dialogs (PermissionController) show in the app's
    window. That's fine and stays the primary consent.
  - Linux: for host resources, the portals add their own consent (Camera,
    Location; microphone has none). Request the portal permission only when the
    Android permission is granted and the resource is first used, and reflect a
    portal denial back as an Android permission denial for that session. All
    files access (`MANAGE_EXTERNAL_STORAGE`) only covers session storage.
- [ ] **Printing** · todo · P2 · sysapp + bridge
  - Android: a `PrintService` in the system app. It receives the PDF from the
    print spooler.
  - Linux: `org.freedesktop.portal.Print` (`PreparePrint` gives the host dialog,
    then `Print` with the PDF fd).
  - Test: Markor (print, verify), Fossify Gallery.
- [ ] **Biometrics and device credential** · todo · P2 · hal + bridge
  - Android: `BiometricPrompt` needs a fingerprint HAL (the emulator's is
    masked). Some apps require `KeyguardManager.isDeviceSecure` (a lock screen
    PIN) before storing keys.
  - Linux: fprintd (`net.reactivated.Fprint`) behind a fingerprint AIDL HAL, or
    polkit authentication as a "device credential" prompt. Until then, apps fall
    back to their passwords.
  - Test: KeePassDX and Aegis (biometric unlock).
- [ ] **Credentials, passkeys and autofill** · todo · P2 · sysapp + bridge
  - Android: Android credential providers and autofill services (KeePassDX,
    Bitwarden) work within Android.
  - Linux: host password managers have no standard API. The Secret portal or
    Secret Service could back an `AutofillService` or
    `CredentialProviderService` in the system app. Low value until there's
    demand.
  - Test: KeePassDX (autofill, passkeys).
- [ ] **Backup** · won't-do · — · —
  - Reason: app data is a host directory (`~/.local/share/linuxapk/data*`), so
    host backup tools already cover it. Android's backup transport has nowhere
    better to go. `linuxapk uninstall --keep-data` covers reinstall.
- [ ] **Game Mode** · todo · P2 · bridge
  - Linux: `org.freedesktop.portal.GameMode` or `com.feralinteractive.GameMode`
    `RegisterGame(pid)`.
  - Android: when the focused app is a game (`appCategory`/`isGame`, already
    read by the CLI), register its host pid.

## Accessibility

- [ ] **AccessibilityNodeInfo to AT-SPI** · todo · P1 · sysapp + bridge
  - Linux: AT-SPI2 on the accessibility bus, through AccessKit's
    `accesskit_unix` adapter, one per Wayland window. Nucleus does exactly this
    (`decorated-window-tao/src/main/native/src/platform/linux/a11y.rs`).
  - Android: an `AccessibilityService` in the system app (enabled through
    `Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES` at setup) with
    `FLAG_RETRIEVE_INTERACTIVE_WINDOWS` and per-display windows
    (`getWindowsOnAllDisplays`). Node trees and events are serialized to the
    bridge, which builds `accesskit::TreeUpdate`s. AT-SPI actions map to
    `performAction`. It must not request touch exploration, which would change
    input behaviour for every app. Effort XL. The strategy schedules it after
    M3.
  - Test: Thunderbird and Markor with Orca.
- [ ] **TalkBack** · won't-do · — · —
  - Reason: Orca on the host is the screen reader. Running both would
    double-speak.
- [ ] **Captions and other accessibility preferences** · todo · P2 · sysapp
  - Android: `CaptioningManager` (`Settings.Secure`
    `accessibility_captioning_*`), and color correction or inversion
    (SurfaceFlinger, per display).
  - Linux: few desktop equivalents; expose them in the manager app.

## Packaging and lifecycle

- [x] **Per-app `.desktop` files and icons** · done · P0 · cli
  - Now: `cli/` (`linuxapk install`), with Android-rendered icons (`IconDump`)
    and localized names (`docs/cli.md`). APK double-click install through
    `linuxapk setup`.
  - Next:
    - entries for apps installed inside Android (see App stores);
    - Flatpak builds would use the DynamicLauncher portal
      (`org.freedesktop.portal.DynamicLauncher` `Install`).
- [x] **App data location** · done · P1 · session
  - Now: `~/.local/share/linuxapk/data[-<instance>]`, the session's `/data`
    (`session/linuxapk-session`).
  - Next: per-app size reporting in the manager app.
- [x] **Uninstall** · done · P0 · cli
  - Now: `linuxapk uninstall [--keep-data]` removes the package, entry and
    icons.
  - Next: uninstall from inside Android (the store or Settings) must remove the
    entry too, through the package callback.
- [ ] **Session idle shutdown** · todo · P1 · session
  - Now: the session runs until `linuxapk session stop`.
  - Next: stop when no windows, foreground services, background-allowed apps or
    pending alarms remain, after a grace period. Save the next alarm for a timer
    first. Planned for M5 (startup and memory budgets).
- [ ] **On-demand start with launch feedback** · partial · P0 · cli + bridge
  - Now: `linuxapk run` starts the session if needed and waits for boot.
  - Next: show the app window, or a splash, at once while the session boots.
    Pass the activation token.
- [ ] **Per-app settings** · todo · P2 · cli + bridge
  - Settings: density multiplier, input mode, background permission, escape as
    back. They go in the manager app (backlog), and Mac.apk has the same idea
    per app as flags.

## Recommended order

The next ten items, by value per effort, assuming `lane/win` (per-app windows)
and `lane/audio` (audio out) land first. Effort: S (days), M (about a week), L
(weeks).

1. **Bridge to system app channel, plus dark mode, time zone and locale** (S–M,
   sysapp + bridge). The socket protocol every later row reuses, proven on the
   cheapest P0 settings rows (portal Settings, timedate1, `LANG`).
2. **App id, title, icon, activation token and default window size** (S,
   windowing + bridge). Makes per-app windows feel native in the taskbar.
   Includes a host cursor on enter (the default arrow).
3. **Notifications with actions** (M, sysapp + bridge). A
   NotificationListenerService to `org.freedesktop.Notifications`; progress and
   inline reply come right after.
4. **Clipboard text both ways** (S–M, sysapp + bridge). `ext-data-control`, with
   a focus fallback.
5. **Open URLs and Custom Tabs via the host browser** (S, sysapp + bridge). The
   default-browser role, CustomTabsService and OpenURI. With the CLI's scheme
   handlers, this unlocks OAuth sign-ins (Thunderbird, Tusky).
6. **IME through `text-input-v3`** (M, sysapp + bridge). Removes the soft
   keyboard and gives non-US text and CJK input through fcitx5/IBus. It also
   fixes the non-US layouts row for text.
7. **SAF to the FileChooser portal, plus the host DocumentsProvider** (M,
   sysapp + bridge). Open, save and tree, with fds passed, never paths.
8. **Minimize/close lifecycle and session keep-alive rules** (M, windowing +
   session). `suspended` stops apps; foreground services and MPRIS keep the
   session running; idle shutdown otherwise.
9. **MediaSession to MPRIS** (S–M, sysapp + bridge). Media keys and desktop
   media controls, with artwork.
10. **Store installs end to end** (S–M, sysapp + cli). F-Droid and Aurora
    installs get `.desktop` entries through the package callback. The confirm
    dialog renders in the store's window.

Next after these:

- virtual evdev devices (L), which unlock gamepads, rumble, real keyboard
  layouts, touchpad gestures and the stylus;
- the cursor layer;
- location;
- camera (L);
- AT-SPI (XL).

## Appendix: sources

### Nucleus

`https://github.com/NucleusFramework/Nucleus`, cloned to `out/research/nucleus`.
These modules are reference implementations; the Linux code is under
`<module>/src/main/native/linux/` and `src/main/kotlin/.../linux/`.

- `darkmode-detector`: portal Settings `org.freedesktop.appearance`
  `color-scheme`. Rows: Dark mode.
- `system-color`: portal Settings `accent-color`. Rows: Accent color.
- `linux-hidpi`: GNOME `text-scaling-factor`, Mutter DisplayConfig. Rows: Font
  scaling, HiDPI.
- `notification-linux`: `org.freedesktop.Notifications`. Rows: Notifications,
  actions, inline reply.
- `media-control`: MPRIS `org.mpris.MediaPlayer2`. Rows: MediaSession to MPRIS.
- `share`: portals FileChooser, OpenURI; `xdg_foreign` parent. Rows: SAF picker,
  share sheet, opening URLs.
- `decorated-window-tao` `platform/linux/a11y.rs`: AccessKit `accesskit_unix` to
  AT-SPI2. Rows: AccessibilityNodeInfo to AT-SPI.
- `decorated-window-tao` `platform/linux/dnd.rs`: GTK/Wayland drag and drop.
  Rows: Drag and drop.
- `decorated-window-tao` `platform/linux/touch.rs`: touch and touchpad
  pinch/rotate. Rows: Touch, touchpad gestures.
- `decorated-window-tao` `platform/linux/ime.rs`: IME caret rectangle
  (`text-input`). Rows: IME.
- `decorated-window-tao` `platform/linux/activation.rs`: `xdg_activation_v1`
  tokens. Rows: Focus and activation.
- `decorated-window-tao` `platform/linux/server_decoration.rs`:
  `org_kde_kwin_server_decoration`, xdg-decoration. Rows: Decorations.
- `decorated-window-tao` clipboard: GTK clipboard. Rows: Clipboard.
- `docs/internals/tao-trackpad.md`: pinch/rotate semantics. Rows: Touchpad
  gestures.
- `energy-manager`: `org.freedesktop.ScreenSaver` and `PowerManagement.Inhibit`,
  GNOME SessionManager inhibit, logind. Rows: Keep awake.
- `system-info`: UPower battery, NetworkManager, NetworkMonitor portal, idle
  monitor. Rows: Battery, network metering, host idle.
- `autolaunch`: Background portal, systemd user units. Rows: Background
  execution and autostart.
- `scheduler`: systemd user timers. Rows: Alarms while the session is stopped.
- `global-hotkey`: GlobalShortcuts portal. Rows: Global shortcuts.
- `launcher-linux`, `taskbar-progress`: Unity `LauncherEntry` (count, progress),
  dbusmenu quicklists. Rows: Badges, progress, app shortcuts.
- `spellcheck`: Hunspell (dlopen). Rows: Spell checking.

### Mac.apk

`https://github.com/kksimp/Mac.apk` and its wiki, cloned to
`out/research/macapk` and `out/research/macapk-wiki`. Its desktop-integration
features, all covered by rows above:

- **Now Playing** (MediaSession): `runtime/src/nowplaying_bridge.mm`.
- **Game controllers** (no rumble): `runtime/src/gamecontroller_bridge.mm`,
  `runtime/java/com/macapk/loader/GamepadBridge.java`.
- **Per-app input modes:**
  - Tilt Control: `TiltControl.java`, `SensorManager.java`;
  - Arrow Keys as D-pad: `ArrowPad.java`;
  - Space Bar Taps Center: `SpaceTap.java`;
  - key-binding profiles: `KeyBindProfiles.java`, `keybinds/`;
  - trackpad pinch and rotate: `TrackpadGestureMapper.java`;
  - the wheel: `WheelScroll.java`.

  The wiki's Controls-and-Input page covers Android TV D-pad apps.

- **Clipboard:** `runtime/src/clipboard_bridge.mm`.
- **Document picker** (open, save, tree): `runtime/src/document_picker.mm`,
  `DocumentPickerHandler.java`, `HostDocumentsProvider.java`.
- **Shared storage with real folders** and **Photos to MediaStore:**
  `SharedStorage.java`, `runtime/src/photos_bridge.mm`.
- **Camera:** `runtime/src/camera_capture.mm`, `CameraBridge.java`.
  **Microphone:** `runtime/src/audio_input.mm`.
- **Location:** `runtime/src/location_bridge.mm`, `LocationBridge.java`, and
  `launcher/LocationHelperMain.swift`.
- **Wallpaper colors for Material You:** `runtime/src/wallpaper_bridge.mm`,
  `monet/`.
- **Dark mode, live:** wiki, Windows-and-Appearance.
- **Host fonts for CJK:** `runtime/src/host_fonts.cpp`.
- **WebView backed by the system web engine:** `runtime/src/webview_bridge.mm`.
- **Google sign-in window, App Link sign-in window and Play licensing:**
  `runtime/src/gms_signin.mm`, `runtime/src/applink_window.mm`, `AppLinks.java`,
  `PlayServices.java`; wiki, Google-Accounts-and-Play-Services.
- **Cross-app launching** (game launchers starting other apps):
  `CrossAppLauncher.java`, `CrossAppDoorbell.java`.
- **App stores with Touch ID per install:** `PackageInstallerBackend.java`,
  `InstallIntentHandler.java`; wiki, App-Stores.
- **Windows:**
  - back button in the title bar, rotate with ⌘R;
  - default size from orientation: `runtime/src/window_policy.cpp`,
    `window_traits.cpp`;
  - Dock icons: `launcher/BundleGenerator.swift`.
- **Share:** `ActivityHost.java` (URL shares to `NSWorkspace openURL:`).
  Notifications are a no-op there (`Context.java`). Our notification rows go
  further.
- **Compatibility list:** `COMPATIBILITY.md` (71 apps; camera and photo gaps
  noted for Aegis, AnkiDroid).

### AOSP 17 (`~/aosp/a17`)

- **Framework services** under
  `frameworks/base/services/core/java/com/android/server/`:
  - `accessibility`, `clipboard`, `display`, `input`, `inputmethod`;
  - `location`, `media`, `notification`, `power`, `sensors`, `vibrator`;
  - `wallpaper`, `locales`, `timezonedetector`, `print` (in `services/print`);
  - `theming` (new `IThemeManager`), `appwindowlayout` (new per-app window
    layout settings), `wm` (`DragDropController`, `setGlobalDragListener`).
- **HAL interfaces** under `hardware/interfaces/`: `audio`, `camera`, `gnss`,
  `health`, `input`, `light`, `power`, `sensors`, `thermal`, `usb`, `vibrator`,
  `biometrics`, `bluetooth`, `nfc`, `drm`.
- **Specific references:**
  - `core/java/android/content/theming/IThemeManager.aidl`;
  - `core/java/android/window/IGlobalDragListener.aidl`;
  - `IInputManager.aidl` (`setPointerIcon`, `requestPointerCapture`, keyboard
    layout APIs);
  - `packages/apps/DocumentsUI/AndroidManifest.xml` (`OPEN_DOCUMENT` filter
    priority 100);
  - `frameworks/native/services/inputflinger/reader/mapper/`
    (`TouchpadInputMapper`, `JoystickInputMapper`, `VibratorInputMapper`,
    `ExternalStylusInputMapper`).

### Waydroid and ATL

- **Waydroid HALs:** `https://github.com/waydroid/android_hardware_waydroid`.
  `hwcomposer/wayland-hwc.cpp` binds `zwp_tablet_v2` (stylus),
  `zwp_pointer_constraints_v1` and `zwp_relative_pointer_v1` (capture),
  `zwp_idle_inhibit_manager_v1` (keep awake) and `wp_fractional_scale_v1`. It
  draws the cursor from a cursor layer, and feeds input through FIFOs that its
  patched EventHub reads. `hwcomposer/WaydroidClipboard.cpp` handles the
  clipboard. Other HALs there: `sensors`, `vibrator`, `lights`, `health`,
  `power`.
- **Waydroid host services:** `https://github.com/waydroid/waydroid`,
  `tools/services/`:
  - `notification_manager.py` (Android notifications to
    `org.freedesktop.Notifications`, 2025);
  - `clipboard_manager.py`;
  - `user_manager.py` (per-app `.desktop` files).
- **ATL** (`https://gitlab.com/android_translation_layer`), from memory: GTK4
  does windowing, input and IME through GtkIMContext. It uses GDK for the
  clipboard and libportal for files and URIs. ATL reimplements the framework, so
  its integration lives in its Java framework stubs rather than at HAL or app
  boundaries. That makes it less reusable for us than Nucleus.

### xdg-desktop-portal interfaces on the dev host

From `/usr/share/dbus-1/interfaces` (KDE Plasma 6):

- Account, Background, Camera, Clipboard, Documents, DynamicLauncher, Email;
- FileChooser, FileTransfer, GameMode, GlobalShortcuts, Inhibit, InputCapture;
- Location, MemoryMonitor, NetworkMonitor, Notification, OpenURI;
- PowerProfileMonitor, Print, ProxyResolver, RemoteDesktop, ScreenCast,
  Screenshot;
- Secret, Settings (with `accent-color`, `contrast` and `reduced-motion`),
  Trash, Usb, Wallpaper.
