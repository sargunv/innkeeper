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
manager and home screen. It starts and draws the lock screen, and built-in apps
such as Settings run in it. The window takes desktop pointer, touch and keyboard
input.

The CLI installs HAPs into an OH profile with `bm`, and `run`, `list`,
`uninstall`, `app stop`, `app clear-cache` and `app clear-data` work there too
([host/cli](../host/cli/README.md)). AMS blocks third-party apps until the lock
screen has been unlocked once since boot, so `run` of such an app fails before
that. An OH profile refuses `open`, files and URIs in `run`, `app info`,
`app set`, adb and the Android-only settings.

The bundle manager installs a HAP whose signature chains to OpenHarmony's public
roots and whose profile is a release profile. A debug profile fails, because it
needs the device's UDID and the kernel's code-signing device, and the session
has neither.

`ohos/build.sh` (`mise run build:ohos`) builds our layer. `mise run build`
stages it in `share/innkeeper/ohos` beside Android's tree. Both guests run the
same static broker, `libexec/innkeeper/innkeeper-brk`.

To test, run `mise run build` and then `mise run test:e2e -- ohos`. The test
fetches the image if needed. On a nested desktop it creates a fresh profile
`e2e-ohos` with the CLI and starts it. Over `exec` it checks that the core
system abilities, screen 0 and SceneBoard come up and that selftest passes
([tests/boot.sh](tests/boot.sh)). It also checks that the window shows. A drag
and a click on the nested desktop's seat then unlock the lock screen and open
Settings. OH's input injector then scrolls Settings and goes home, and
render_service must not have restarted since boot
([tests/render.sh](tests/render.sh)). A key press reaches MMI, and the CLI
installs and starts the image's calculator HAP.

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

SceneBoard shows the lock screen about 30 s after samgr starts. A drag up the
window unlocks it. OH's own injector also drives it over `exec`.
`uinput -T -m 360 1150 360 300 300` swipes up to the home screen, and
`uinput -T -c 90 200` taps Settings there.

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
  as the main screen. render_service composes every layer
  (`COMPOSITION_CLIENT`). Each committed client target goes to the bridge's
  display service as an opaque LINEAR dma-buf, through the shared
  [bridge client](../guest/bridge-client/include/innkeeper/bridge_client.h).
  `DISPLAY_WINDOW` gives the display its own desktop window. Commit waits up to
  two frames for the compositor to release the other targets, so render_service
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
  ([supervisor/README.md](../supervisor/README.md#openharmony)).
