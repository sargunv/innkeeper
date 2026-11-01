# ohos

This directory holds an experimental OpenHarmony userspace, a second guest on
the same supervisor as Android. Issue #191 tracks its feasibility and
milestones. A profile picks OpenHarmony when it is created, with
`innkeeper profile create --system ohos harmony` or the manager's New Profile.
The launcher starts it the same way it starts Android
([host/launcher/src/ohos.rs](../host/launcher/src/ohos.rs)).

The pinned image boots on the host kernel in the profile's Flatpak sandbox. OH's
init starts its services, samgr comes up, and OH's core system abilities
register with it. render_service composes OH's screen through our display HDI,
and the bridge shows the screen in a desktop window. SceneBoard is OH's window
manager and home screen. Boot ends on its home screen, and built-in apps such as
Settings run in it. The window takes desktop pointer, touch and keyboard input.
OH's audio plays through the desktop's PipeWire.

The CLI installs HAPs into an OH profile with `bm`, and `run`, `list`,
`uninstall`, `app info`, `app stop`, `app clear-cache` and `app clear-data` work
there too ([host/cli](../host/cli/README.md)). `run` brings the app's window to
the front. `run` with a URI opens it in the app as a browser link. An OH profile
refuses `open`, files in `run`, `app set`, adb and the Android-only settings.

The bundle manager installs a HAP whose signature chains to OpenHarmony's public
roots. A release profile installs on any device. A debug profile names the UDIDs
of the devices it installs on. Each profile has a UDID, which
`innkeeper profile list` shows. The launcher derives it from the profile's name
and sets it as `const.product.udid`. installd hands a debug profile's
certificate chain to the kernel's code-signing device, `/dev/code_sign`, which
the broker serves.

`ohos/build.sh` (`mise run build:ohos`) builds our layer. `mise run build`
stages it in `share/innkeeper/ohos` beside Android's tree. Both guests run the
same static broker, `libexec/innkeeper/innkeeper-brk`.

## Probe HAP

`ohos/probe` is a small app for the tests, `dev.sargunv.innkeeper.probes`. Its
launcher ability shows one page with a text field. Started with
`--ps mode capture`, it records half a second from the microphone and logs the
loudest sample to hilog. `mise run build:ohos-probe` builds it into
`out/test/probes/InnkeeperProbes.hap`, and `mise run build` runs that task too.

The build uses the OpenHarmony SDK's own tools and no hvigor, DevEco or account.
[ohos/sdk.env](sdk.env) pins the SDK release that matches the image, and
`ohos/sdk.sh` fetches the parts the build uses into `$OHOS_SDK_HOME`
(`~/.cache/innkeeper/ohos-sdk`). The SDK archive holds one zip per component,
and the build needs the first two, so a fetch reads about 180 MB of the
archive's 3.7 GB. These parts are open source under Apache 2.0.

- restool compiles the resources and resolves the resource references in
  `module.json`.
- The SDK's ArkTS compiler, ets-loader, compiles `ets/` into `modules.abc`.
  ets-loader is a set of rollup plugins written for hvigor's rollup fork.
  `ohos/arkts.js` runs them on upstream rollup 3 (pinned beside the SDK) and
  adds the few plugin-context members and the module resolution that the fork
  provides.
- The packing tool makes the HAP. A test signs it with `ohos/sign.sh`, which
  uses the SDK's public keys and a debug profile for the test profile's UDID, as
  a developer signs an app under test. The build copies the SDK files that
  sign.sh uses to `out/test/probes/ohos-sign`.

`ohos/hap.sh` runs these steps for both of our HAPs, the probe and
[Innkeeper System](#innkeeper-system).

To test, run `mise run build` and then `mise run test:e2e -- ohos`. The test
fetches the image if needed. On a nested desktop it creates a fresh profile
`e2e-ohos` with the CLI and starts it. Over `exec` it checks that the core
system abilities, screen 0 and SceneBoard come up and that selftest passes
([tests/boot.sh](tests/boot.sh)). It also checks that the window shows. Session1
must report the session booted only once `bootevent.boot.completed` is set, and
`bm dump` must show Innkeeper System as a kept-alive system app. A click on the
nested desktop's seat then opens Settings, and MMI's cursor must be on the panel
pixel under the seat's pointer. A wheel detent scrolls Settings' list. OH's
input injector then scrolls Settings and goes home, and render_service must not
have restarted since boot ([tests/render.sh](tests/render.sh)). A key press
reaches MMI. A file written to `/storage/Users/currentUser/Download` lands in
the profile's data directory. The probe HAP, signed for the profile's UDID, then
installs and shows in `innkeeper list`, and `innkeeper run` puts its window on
screen with input focus. A click on the probe's text field starts Innkeeper
Keyboard, and keys typed in a French layout on the nested desktop type "aé" in
the field. OH's music volume must be at its maximum, and a 440 Hz tone that
[tests/tone.c](tests/tone.c) plays through OH's audio API must reach a private
null sink on the nested desktop. Last, root grants the probe the microphone with
`atm perm -g`, the host plays a tone into the null sink, and the probe must
record it from the sink's monitor.

Init logs to the profile's kmsg log (`innkeeper profile log`). Once hilogd runs,
services log to hilog. hilog's buffer is 256 KiB and boot fills it within a
minute, so read early boot from the kmsg log.

`mise run session -- exec` and `shell` run commands as root in OH's
`/system/bin/sh`. They go through the exec relay from android/system/exec, built
static against musl and started by `sys_prod/etc/init/innkeeper_exec.cfg`. Its
native token holds the permissions of hdcd, the image's `hdc shell` daemon. With
them, `bm` installs, lists and removes bundles, `aa` starts and stops abilities,
and `hidumper -ls`, `hilog -x` and `ps -A` work.

## Innkeeper System

`ohos/system-app` is Innkeeper System, `dev.sargunv.innkeeper.system`, the
counterpart of Android's system app. It connects OpenHarmony to the bridge and
reports boot completion. Issue #312 tracks the features that build on it.

- **App.** The app is ArkTS. Its main element is a ServiceExtensionAbility,
  `SystemService`, so it has no window and no home-screen icon.
- **Preinstall.** OH's bundle manager reads `etc/app/install_list.json` in each
  config layer, and ours names `/sys_prod/app/InnkeeperSystem`. BMS installs the
  app at a profile's first boot. `install_list_capability.json` makes the app
  kept alive and a singleton, and allows its service extension, which BMS
  otherwise refuses. The entry matches the app by its appId, which is the bundle
  name and the signing key's public EC point. hap-sign-tool issues a new app
  certificate at each signing, so a certificate fingerprint wouldn't match. AMS
  starts a kept-alive app's main element at boot and restarts it if it dies.
- **Signing.** `ohos/sign.sh --system` signs with the SDK's public keys and a
  release profile at `system_basic` with `hos_system_app`. A release profile
  names no device, so one HAP serves every profile.
- **Native module.** ArkTS sockets have no `SOCK_SEQPACKET` and pass no fds, so
  `native/` is a C N-API module, `libinnkeeper.so`, on
  [guest/bridge-client](../guest/bridge-client/). It gives ArkTS `open`,
  `onEvent`, `request`, `blob` and `waitParameter`, which
  `oh_modules/libinnkeeper.so/index.d.ts` declares. A reader thread per
  connection reconnects after the bridge restarts. The module builds as the VDIs
  do and links the image's `libace_napi` and `libbegetutil`, which a system
  app's native code may load. OH's N-API follows Node's C ABI, so the build uses
  Node's N-API headers.
- **Boot completion.** The app waits for `bootevent.boot.completed`, which init
  sets after SceneBoard sets `bootevent.launcher.ready`. It then sends
  `BOOT_COMPLETED` on the bridge's apps channel, and again on each new
  connection. Session1's `State()` reports the session booted from then on, so
  the manager shows an OH profile as running once its home screen is up. The CLI
  reads the same parameter over `exec`.
- **Input method.** The app's InputMethodExtensionAbility, Innkeeper Keyboard,
  connects OH's text fields to the host's input method
  ([Text input](#text-input)).
- **Build.** `mise run build:ohos-system-app` builds the HAP and stages it with
  the two JSON files. The SDK's declarations leave out the system APIs, such as
  ServiceExtensionAbility. `ohos/system-app/sdk.sh` makes a copy of the SDK in
  `$OHOS_SDK_HOME/<version>-system` with the full declarations from
  interface_sdk-js at the release's tag, processed by that repository's own
  scripts as the SDK's build does. [system-app/sdk.env](system-app/sdk.env) pins
  them and the N-API headers by checksum. ets-loader reports each use of a
  system API as a warning.
- **Updates.** BMS scans the preinstalled apps at a profile's first boot and
  after a system update, which it detects from the image's const parameters.
  When the launcher composes a profile's root, which it does whenever our layer
  or the image changes, it gives init `persist.bms.test-upgrade=true` as a
  parameter default for the next boot
  ([launcher](../host/launcher/src/ohos.rs)). BMS then scans the preinstalled
  apps, and it replaces a HAP whose build hash changed. The packing tool writes
  that hash because `module.json` sets `generateBuildHash`. The launcher removes
  the default at the following start, so later boots skip the scan, which costs
  about 0.8 s.

The connection's host tests (`native/tests`) run in `mise run test`.

## Image

[ohos/pin.toml](pin.toml) pins Oniro's x86_64 emulator release, which is built
on an OpenHarmony release. The user's machine downloads it from GitHub when an
OpenHarmony profile first starts. It is not redistributed or committed. No
OpenHarmony source publishes an anonymous x86_64 standard-system image, so this
is the closest match to Android's CI pin.

The shared fetcher [image/fetch.sh](../image/fetch.sh) downloads the image. It
uses the same pin schema, cache, leases and pruning as Android's.
`image/extract-ohos.sh` then extracts `system.img` and `vendor.img` with
`image/fs-extract.sh`'s static debugfs. The extracted trees are read-only, as
they would be on a mount, and extraction needs no loop mounts or root. The guest
kernel, ramdisk and other images are discarded, because the guest runs on the
host kernel in our sandbox and not in QEMU. The partitions go to
`images/<name>/img/` in the image cache: `system/` holds system.img's
system-as-root tree, and `vendor/` holds the vendor partition.

To bump the pin, take a newer `oniro_emulator.zip` from
[device_board_oniro releases](https://github.com/eclipse-oniro4openharmony/device_board_oniro/releases).
`gh release view <tag> --json assets` gives its size and digest. Update `name`,
`version`, `url`, `size` and `sha256`, then run `mise run test:e2e -- ohos`.

## Display

```sh
INNKEEPER_PROFILE=harmony tests/nested.sh bash -c \
  'tools/dev/innkeeper.sh profile create --system ohos harmony && tools/dev/session.sh start -f'
```

Screen 0 opens as one window the size of the profile's display preset. The
presets are `small-phone` (720x1280 at 320 dpi, the default) and `medium-phone`.
OH has no windowed mode, and a tablet's landscape layout is untested. Run OH on
a nested desktop (`tests/nested.sh`) and not on your own.

SceneBoard shows the home screen about 30 s after samgr starts. A new profile's
lock screen is off for user 100, as Android's lock screen is off by default
(`ro.lockscreen.disable.default`). OH's screenlock service keeps a per-user
"disabled" flag in `/data/service/el1/public/screenlock/screenlock_state.xml`,
and SceneBoard doesn't lock the screen while the flag is set. The launcher
writes the file before the first boot. When the user sets a PIN in Settings, the
service clears the flag, and the lock screen asks for the PIN from the next
restart on. OH's PIN and user authentication HDIs keep their data in
`/data/chipset/el1/public/pinauth` and `userauth`. The image creates neither
directory, so `sys_prod/etc/init/innkeeper_useriam.cfg` does. The desktop has
its own lock screen. OH's own injector also drives the screen over `exec`.
`uinput -T -c 90 200` taps Settings on the home screen.

- **Display.** The launcher sets the panel from the profile's preset in two
  places. The composer reads the `const.innkeeper.display` parameter
  (`<width>x<height>@<dpi>`, in `/vendor/etc/param/innkeeper.para`). DMS reads
  the `<dpi>` in its config, which is the only place it takes density from. The
  image's value of 160 would make a small phone a tablet 720 vp wide. At 320 dpi
  a small phone is 360x640 vp, which matches `const.product.devicetype`
  (`default`, a phone) and the window's size on the desktop. The composer also
  reads the panel's physical size in millimetres from
  `const.innkeeper.display.size` ([Input](#input)).
- **GPU.** The broker's DRM policy lets the guest open only the profile's
  selected render node. Without it, the image's Mesa would open the host's card
  node, and its composer would drive the host display's KMS. That Mesa ships
  iris, crocus, nouveau, virtio_gpu and llvmpipe, but not radeonsi, and CI has
  no GPU. render_service therefore draws with llvmpipe
  (`LIBGL_ALWAYS_SOFTWARE`). GPU rendering needs a Mesa built for OH's musl,
  which is milestone 8 of #191.
- **Native fences.** The image's llvmpipe finishes every GL context in the
  process whenever it exports a native fence fd, and EGL exports one on each
  swap. render_service draws on several threads, and a swap on one thread would
  flush a context that another thread is drawing with, which crashes llvmpipe.
  The shim makes the dma-buf sync file export (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`)
  fail, so llvmpipe offers no native fences. EGL's software swap waits for its
  own context's rendering before it queues the buffer.
- **Ashmem.** The HDI's command queues use ashmem, as most OH shared memory
  does. The broker emulates ashmem with memfds
  ([supervisor/README.md](../supervisor/README.md#openharmony)).

The display HDI (`vendor/display/`) is two VDI libraries that replace the
image's. The image's VDIs drive DRM/KMS and allocate dumb buffers on
`/dev/dri/card0`. The launcher's composed root installs ours in `/vendor/lib64`.
The image's own `composer_host` and `allocator_host` load them, and no image
file changes.

- **Composer** (`libdisplay_composer_vdi_impl.z.so`). It reports one display, 0,
  at 60 Hz in the panel's mode. The display is a built-in panel, so OH uses it
  as the main screen. render_service's uni-render output is one full-screen
  device layer, and the composer shows that buffer itself. With any other set of
  layers, render_service composes every layer (`COMPOSITION_CLIENT`) into a
  client target with GL, which costs about 60% more render_service CPU on
  llvmpipe. Each shown buffer goes to the bridge's display service as an opaque
  LINEAR dma-buf, through the shared
  [bridge client](../guest/bridge-client/include/innkeeper/bridge_client.h).
  `DISPLAY_WINDOW` gives the display its own desktop window. Commit waits up to
  two frames for the compositor to release the other buffers, so render_service
  does not draw into a buffer that is on screen. A 60 Hz timer provides vsync.
  Without a desktop (`start --headless`), the display is headless.
- **Buffers** (`passthrough/libdisplay_buffer_vdi_impl.z.so`). Buffers are
  sealed memfds that `/dev/udmabuf` turns into dma-bufs, like Android's CPU
  buffers. They are LINEAR with a 256-byte row pitch, mappable, and importable
  by llvmpipe and the compositor. The supported formats are RGB 8888, 565, 888
  and 1010102, and YUV 4:2:0.
- **Build.** `build.sh` compiles with the NDK's clang for `x86_64-linux-musl`,
  using the build container's musl headers. Stub libraries give the linker the
  `libc.so`, `libc++.so`, `libhilog.so` and `libbegetutil.z.so` sonames, and the
  image's libraries resolve the symbols at load time. The C++ code does not
  include libc++ headers. It writes the few `std::vector` and `std::string`
  fields in the ABI in place, using libc++'s `std::__h` layout. The buffer VDI's
  vtable comes from declaring OH's interface in the same order.
- **One window for all apps.** OH composes every window into one screen buffer
  before the HDI sees it. Layers reach the composer separately only when
  render_service offloads them to hardware composition. It does that for
  self-drawing surfaces such as video and XComponent, but not for ordinary ArkUI
  windows. SceneBoard's window manager puts apps on screen 0, and it also owns
  OH's floating and freeform modes. A window per app would need one OH screen
  per app window, plus an OH window policy that places each app on its own
  screen. The bridge therefore shows screen 0 as one phone-frame window.

## Input

OH's multimodal input service (MMI) reads evdev devices through libinput, as it
would on a kernel. The broker serves the session-local devices that Android uses
([docs/input.md](../docs/input.md#devices)). MMI finds them in `/dev/input`, and
the broker answers its sysfs lookups for them
([supervisor/README.md](../supervisor/README.md#openharmony)).

The window has no window manager on the input service, so the bridge writes its
seat to the broker's devices (`host/bridge/src/input/direct.rs`). The pointer is
the Innkeeper Mouse, with hover and the right button's menus. A host touch is a
contact on the Innkeeper Touchscreen.

The composer reports a hardware cursor, so render_service leaves OH's cursor out
of the frame, and MMI gives each cursor image to the composer's
`UpdateHardwareCursor`. MMI draws the image in a 512 by 512 buffer with the
cursor's hotspot at the centre. The composer crops it and sends it to the bridge
when it changes, and the bridge shows it as the host's cursor over the window.
The host compositor draws it at the host pointer, so it doesn't wait for OH's
next frame, and OH's shapes such as the I-beam carry over. render_service also
gives the composer a layer of type `LAYER_TYPE_CURSOR`, which the composer
leaves out.

The composer reports the panel's physical size at 160 dpi, a desktop monitor's
density, so DMS gives apps an x and y dpi near 160. The density that OH lays out
with stays the profile's dpi.

MMI moves its cursor by relative motion, and it scales the motion. For a display
with a physical size, it multiplies by a factor for the screen's diagonal, which
is 1 from 8 to 17 inches, and by its dynamic curve. The curve depends on the
pointer speed and on the display's ppi, and MMI reads it from
`etc/multimodalinput/pointer_motion_acceleration_config.json`. config_policy
looks that file up in `sys_prod` before `system`. The launcher writes one there
with a flat curve whose standard ppi is the panel's, so one unit of motion moves
the cursor one panel pixel. Both phone presets have a diagonal in the range at
160 dpi. The host has already accelerated the pointer, and OH's pointer speed
setting has no effect. The bridge sends the difference between the pixel under
the host pointer and the pixel under OH's cursor. When the pointer enters the
window, a large negative motion first moves OH's cursor into the top-left
corner, where MMI stops it. MMI drops motion that comes before it has opened the
mouse and has its display. That happens when the window appears under the
pointer during boot, so the cursor goes through the corner again before each
press that starts a click.

MMI reads libinput's legacy wheel events, whose value is the REL_WHEEL count
times the mouse's click angle. The broker gives the mouse an angle of 1 degree,
a fifteenth of a detent, so the fractions of a high-resolution wheel reach OH. A
touchpad's scroll becomes wheel degrees, at 63 surface units to the detent,
which is how far one detent scrolls an OH list.

Keys go to the Innkeeper Keyboard device while the window has keyboard focus.
They keep their evdev codes, and OH maps them with its own US keymap. MMI
repeats held keys itself, so the bridge drops the compositor's repeats.

### Text input

Innkeeper System's input method, Innkeeper Keyboard (`ets/ime/HostIme.ets`),
connects OH's text fields to the host's input method. It works like Android's
([docs/input.md](../docs/input.md#text-input)) and speaks the same `ti_*` lines
with the bridge, so the host's IBus or Fcitx composes text for OH editors.

- **Default.** `sys_prod/etc/param/innkeeper_ime.para` sets
  `persist.sys.default_ime` to the input method. The image's file names its own
  keyboard, and OH reads our file after it. A choice the user makes in Settings
  persists over the default. The input method creates no panel, so no soft
  keyboard covers the window.
- **Editors.** OH's `inputStart` enables text input with the editor's content
  type, and `inputStop` disables it. A password field gets the password purpose
  and the hidden-text hint. The surrounding text and the cursor's rectangle
  follow the editor, so the host's candidate window opens at the text cursor.
  ArkUI's TextInput reports the pattern `PATTERN_NULL`, which the input method
  treats as plain text. `cursorContextChange` gives the cursor's top, although
  the SDK's declaration says bottom.
- **Results.** The host's commits insert text, its preedit shows as OH's preview
  text, and its deletions delete around the cursor. OH's `deleteForward` deletes
  before the cursor and `deleteBackward` after it. OH has no cursor inside a
  preview.
- **Layout.** A key that types text reaches the input method before the editor,
  as on Android. The input method consumes it and commits what the key types
  under the host's layout and compose (`ets/ime/KeyText.ets`). The bridge's
  `key` line carries that text, and the bridge sends the line before it writes
  the key, so the text is there first. Keys without text, such as arrows, Enter
  and Backspace, and chords with Ctrl, Alt or Meta reach the editor as keys.
  Shortcuts keep OH's US key positions.
- **Order.** Keys and the text that replaces them reach the editor in the order
  they were typed. Key events less than about a millisecond apart can arrive out
  of order. Typing doesn't produce such gaps, and a test that sends keys in a
  burst can.

`ohos/system-app/tests` runs the input method's line and key logic under Node
with `mise run test`.

## Audio

OH's audio service mixes every app into one render on the HDI's `primary`
adapter, and it records from that adapter's capture. Our audio VDI
(`vendor/audio/`) carries both through the bridge's PipeWire streams. It uses
the blocking PCM device from
[guest/audio-client](../guest/audio-client/include/innkeeper/pcm.h), as
Android's audio HAL does ([android/vendor/audio](../android/vendor/audio/)).

The image's `audio_host` loads `libaudio_manager_service_6.1.z.so`. That library
loads the image's ALSA implementation by default. With `ohos.audio.hdi_impl=vdi`
it loads OH's IDL-to-VDI adapter, which loads
`/vendor/lib64/libaudio_primary_impl.z.so`. The image has no library at that
path, and the launcher's composed root installs ours there. The parameter is in
`sys_prod/etc/param/innkeeper_audio.para`.

- **Format.** Renders and captures are 16-bit PCM in one or two channels. The
  image's audio policy runs the primary pipes at 44.1 kHz. The launcher's copy
  of `/vendor/etc/audio/audio_policy_config.xml` runs them at 48 kHz, the
  desktop's usual rate, so neither OH nor PipeWire resamples the mix.
- **Volume.** OH's system volumes stay at their maximum, so OH's volume and the
  desktop mixer don't stack. `const.multimedia.audio.fixedvolume` stops volume
  changes, and the launcher's `sys_prod/etc/audio/audio_volume_config.xml` sets
  each volume type's default level to its maximum. The VDI reports a volume that
  OH sets but doesn't apply it. An app's own stream volume still applies.
- **Buffers and latency.** OH writes bursts of 256 frames (5.3 ms). The playback
  ring holds two 5 ms periods, and the bridge asks PipeWire for another 20 ms,
  so the VDI reports 30 ms of latency. The capture ring holds eight 10 ms
  periods to absorb scheduling jitter.
- **Pacing and position.** Writes block on ring space, which follows PipeWire's
  clock. The render position is the frames written minus the bridge's estimate
  of frames not yet presented, with a `CLOCK_MONOTONIC` time.
- **Mixer name.** The desktop shows the stream as "OpenHarmony". This build of
  OH's audio service sends the HDI neither the playing app nor the stream usage.
- **Capture.** OH's privacy service refuses the microphone to processes that
  aren't HAPs, so a native tool can't record. The e2e test records with the
  probe HAP. The bridge records the default source, or `INNKEEPER_AUDIO_SOURCE`.
  Microphone mute zeroes the captured samples.
- **No bridge.** Without a bridge, playback runs on a virtual clock and capture
  returns paced silence, as on Android.

Host unit tests (`vendor/audio/tests`) drive the VDI through the tables that
OH's adapter calls. They cover the adapter and its ports, rejected formats,
per-render tables, burst pacing, latency, position, frame counts, volume and
paced capture.

## How it boots

OH boots the same way as Android's session
([docs/session.md](../docs/session.md)). The launcher composes a root on disk
and symlinks to it from the sandbox's own `/`. It then starts the broker, and
the broker starts init. The kmsg log, bridge, control socket and lifecycle are
the same as Android's. OH's session does not stop when idle, because only
Android's system app reports activity.

- **Root.** `system` and `vendor` are the extracted partitions as hardlinks or
  reflink copies. Our VDIs and the panel's settings go over `vendor`, and
  `sys_prod` is our layer. The image leaves OH's product partition empty, but
  that partition is on init's cfg path and the musl linker's search path. Our
  files go there, so the image does not change. The sandbox keeps its `/usr`,
  `/bin` and `/etc` for the glibc side. `/etc` also gets OH's other entries, and
  `/lib` gets the musl loader.
- **Init.** The launcher runs `/system/bin/init --second-stage`, because the
  image's first stage only mounts block devices and switches root. The shim
  makes `getpid()` return 1 in init, which refuses to run otherwise. It makes
  mount and namespace calls succeed as no-ops, because Flatpak's seccomp filter
  refuses them before ours sees them. It makes priority raises best-effort. A
  `setns` into a mount namespace still moves init to that namespace's `/`, as
  the kernel does. Init's `mksandbox` changes into its sandbox first, and
  services inherit init's working directory.
- **Apps.** appspawn gives each app a mount namespace. It bind-mounts into
  `/mnt/sandbox/<user>/<bundle>` and then calls `pivot_root` there. Apps find
  their own files only at those paths, and `/data/storage/el1/bundle` holds the
  app's code. In appspawn and the other spawners, the shim turns each bind mount
  into a symlink at its mount point. `pivot_root` keeps the sandbox's
  `/data/storage` open on a fixed fd. The session's `/data/storage` links to
  `/proc/self/fd/<fd>`, so each app process sees its own. The rest of an app's
  root is the session's.
- **Users.** When user 100 starts, storage_daemon mounts its storage views under
  `/storage` and `/mnt`. A failed mount would leave the user unverified, and AMS
  would then refuse to start any third-party app. The image's storage_daemon
  runs without hmdfs, so each view is a bind or sharefs mount of a directory in
  `/data`. In storage_daemon, the shim turns these mounts into symlinks to their
  sources. The user's files are in `/data/service/el2/100/hmdfs/account/files`,
  and `Docs` there holds Documents, Download and Desktop. appspawn binds the
  `Docs` view into apps' sandboxes at `/storage/Users/currentUser`, and the
  launcher links the session's `/storage/Users` to the same view. Apps and
  FileManager's "My phone" see the same files, and they stay in the profile's
  data directory on the host. hmdfs's distributed and cloud views stay empty.
- **Inodes.** BMS checks for free inodes before each install. btrfs has no fixed
  inode table and reports no free inodes, so BMS would refuse SceneBoard and the
  larger apps. On such filesystems the shim's `statfs` reports one free inode
  per free block.
- **Paths.** `/data` is a symlink to the profile's directory on the host.
  installd resolves each code path with `realpath` and refuses any path outside
  `/data/app/el1/bundle/`. The shim's `realpath` returns `/data` in place of the
  host directory, so installs and other path checks see guest paths.
- **Broker.** OH runs the same static musl broker as Android, because OH's root
  has no Bionic. With `INNKEEPER_GUEST=ohos` it also traps `open(2)`, because
  musl does not use `openat`. It serves `/etc/passwd` and `/etc/group` from
  `/system/etc`, handles OH's binder ioctls and serves `/dev/access_token_id`
  and `/dev/code_sign`
  ([supervisor/README.md](../supervisor/README.md#openharmony)).
