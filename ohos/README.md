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
the front. An OH profile refuses `open`, files and URIs in `run`, `app set`, adb
and the Android-only settings.

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
launcher ability shows one page. `mise run build:ohos-probe` builds it into
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
  `ohos/probe/arkts.js` runs them on upstream rollup 3 (pinned beside the SDK)
  and adds the few plugin-context members that the fork provides.
- The packing tool makes the HAP. A test signs it with `ohos/probe/sign.sh`,
  which uses the SDK's public keys and a debug profile for the test profile's
  UDID, as a developer signs an app under test. The build copies the SDK files
  that sign.sh uses to `out/test/probes/ohos-sign`.

To test, run `mise run build` and then `mise run test:e2e -- ohos`. The test
fetches the image if needed. On a nested desktop it creates a fresh profile
`e2e-ohos` with the CLI and starts it. Over `exec` it checks that the core
system abilities, screen 0 and SceneBoard come up and that selftest passes
([tests/boot.sh](tests/boot.sh)). It also checks that the window shows. A click
on the nested desktop's seat then opens Settings. OH's input injector then
scrolls Settings and goes home, and render_service must not have restarted since
boot ([tests/render.sh](tests/render.sh)). A key press reaches MMI. The probe
HAP, signed for the profile's UDID, then installs and shows in `innkeeper list`,
and `innkeeper run` puts its window on screen with input focus. OH's music
volume must be at its maximum, and a 440 Hz tone that
[tests/tone.c](tests/tone.c) plays through OH's audio API must reach a private
null sink on the nested desktop.

Init logs to the profile's kmsg log (`innkeeper profile log`). Once hilogd runs,
services log to hilog. hilog's buffer is 256 KiB and boot fills it within a
minute, so read early boot from the kmsg log.

`mise run session -- exec` and `shell` run commands as root in OH's
`/system/bin/sh`. They go through the exec relay from android/system/exec, built
static against musl and started by `sys_prod/etc/init/innkeeper_exec.cfg`. Its
native token holds the permissions of hdcd, the image's `hdc shell` daemon. With
them, `bm` installs, lists and removes bundles, `aa` starts and stops abilities,
and `hidumper -ls`, `hilog -x` and `ps -A` work.

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
  (`default`, a phone) and the window's size on the desktop.
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
seat to the Innkeeper Touchscreen (`host/bridge/src/input/direct.rs`). A click
is a tap, and a drag with the left button is a swipe. Oniro's emulator also
turns its pointer into a touchscreen. MMI moves its cursor by each event's
relative motion, which an absolute pointer doesn't report. A host touch is a
contact on the touchscreen.

Keys go to the Innkeeper Keyboard while the window has keyboard focus. They keep
their evdev codes, and OH maps them with its own keymap, so the host's layout
doesn't apply. MMI repeats held keys itself, so the bridge drops the
compositor's repeats. The wheel and text input from the host's input method
don't reach OH yet.

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
  aren't HAPs, so a native tool can't record. The bridge records the default
  source, or `INNKEEPER_AUDIO_SOURCE`. Microphone mute zeroes the captured
  samples.
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
  `/storage` and `/mnt/data`. The shim makes these mounts succeed as no-ops, so
  the views are plain directories. A failed mount would leave the user
  unverified, and AMS would then refuse to start any third-party app.
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
