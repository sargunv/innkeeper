# ohos

An experimental OpenHarmony userspace as a second guest, on the same supervisor
as Android (feasibility and milestones: #191). It runs only from a source
checkout: no profile can pick it yet, so the shipped app says Android only. The
pinned image boots in a test Flatpak sandbox on the host kernel: OH's init
starts its services, samgr comes up and OH's core system abilities register with
it. render_service composes OH's screen through our display HDI, and with the
bridge it shows in a desktop window. SceneBoard, OH's window manager and home
screen, starts and draws the lock screen; built-in apps such as Settings run in
it. There is no desktop input or app install yet. `ohos/build.sh`
(`mise run build:ohos`) builds our layer, which `mise run build` stages in
`share/innkeeper/ohos` beside Android's tree; both guests run the same static
broker, `libexec/innkeeper/innkeeper-brk`. Nothing starts OH but `run.py`.

`mise run build` first, then `mise run test:e2e -- ohos`: it fetches the image
if needed and boots it as profile `e2e` until SceneBoard is ready (scratch in
`out/dev/flatpak/e2e/`, data wiped at each run). `ohos/run.py` alone uses
profile `ohos`. Init's log is `ohos-kmsg.log` there; once hilogd runs, services
log to hilog instead. To look around, write a script for OH's `/system/bin/sh`
and pass it with `ohos/run.py --probe <script>`: it runs as root, with a native
token holding `ohos.permission.DUMP`, once `bootevent.samgr.ready` is set, and
its output comes back (`hidumper -ls`, `hilog -x`, `ps -A`). hilog's buffer is
256 KiB and boot fills it within a minute: read early boot from `ohos-kmsg.log`.

## Image

[image/ohos/pin.toml](../image/ohos/pin.toml) pins Oniro's x86_64 emulator
release, built on an OpenHarmony release. It is downloaded from GitHub on the
developer's machine and never redistributed or committed. No OpenHarmony source
publishes an anonymous x86_64 standard-system image, so this is the closest to
Android's CI pin. `image/ohos/fetch.sh` downloads the zip (resumable, size and
SHA-256 checked), then extracts `system.img` and `vendor.img` with
`image/fs-extract.sh`'s static debugfs, read-only as on a mount: no loop mounts,
no root. The guest kernel, ramdisk and other images are discarded; the guest
runs on the host kernel in our sandbox, never in QEMU. The cache is
`out/ohos/images/<name>/img/` (`system/`, system.img's system-as-root tree, and
`vendor/`).

To bump the pin, take a newer `oniro_emulator.zip` from
[device_board_oniro releases](https://github.com/eclipse-oniro4openharmony/device_board_oniro/releases)
(`gh release view <tag> --json assets` gives the size and digest), update
`name`, `version`, `url`, `size` and `sha256`, then run
`mise run test:e2e -- ohos`.

## Display

```sh
INNKEEPER_PROFILE=ohos tests/nested.sh ohos/run.py --display --timeout 60
```

`--display` also starts the bridge in the sandbox, so screen 0 opens as one
720x1280 window (log: `ohos-bridge.log`). Run it on a nested desktop
(`tests/nested.sh`), never on your own.

SceneBoard shows the lock screen about 30 s after samgr. Until desktop input
lands, OH's own injector drives it from a probe:
`uinput -T -m 360 1150 360 300 300` swipes it open to the home screen, and
`uinput -T -c 90 200` taps Settings there.

- **Display.** `sandbox.sh` defines the panel once (`display=720x1280@320`): the
  composer reports that mode, and DMS gets the same dpi in its config's `<dpi>`,
  the only place it takes density from (the image's 160 would make it a 720 vp
  wide tablet). At 320 dpi it is 360x640 vp, a phone like
  `const.product.devicetype` (`default`), matching the window's size on the
  desktop.
- **GPU.** Only the first render node opens (the broker's DRM policy): the
  image's Mesa otherwise opens the host's card node, and its composer would
  drive the host display's KMS. That Mesa ships iris, crocus, nouveau,
  virtio_gpu and llvmpipe but no radeonsi, and CI has no GPU, so render_service
  draws with llvmpipe (`LIBGL_ALWAYS_SOFTWARE`). A GPU needs a Mesa built for
  OH's musl (#191's milestone 8).
- **Ashmem.** The HDI's command queues, like most OH shared memory, are ashmem,
  which the broker emulates with memfds
  ([supervisor/README.md](../supervisor/README.md#openharmony)).

The display HDI (`vendor/display/`) is two VDI libraries in place of the image's
(which drive DRM/KMS and allocate dumb buffers on `/dev/dri/card0`), installed
in `/vendor/lib64` by `sandbox.sh`'s overlay. The image's own `composer_host`
and `allocator_host` load them; no image file changes.

- **Composer** (`libdisplay_composer_vdi_impl.z.so`). One display, 0, at 60 Hz,
  reported as a built-in panel so it is OH's main screen
  (`INNKEEPER_OHOS_DISPLAY`, `<width>x<height>@<dpi>`, overrides the panel).
  render_service composes every layer (`COMPOSITION_CLIENT`); each committed
  client target goes to the bridge's display service as a LINEAR dma-buf, shown
  opaque, through the shared
  [bridge client](../guest/bridge-client/include/innkeeper/bridge_client.h).
  `DISPLAY_WINDOW` gives the display a desktop window of its own. Commit waits
  (up to two frames) for the compositor to release every other target, so
  render_service never draws into one being shown. A 60 Hz timer is the vsync.
  Without a bridge (`mise run test:e2e -- ohos`) the display is headless.
- **Buffers** (`passthrough/libdisplay_buffer_vdi_impl.z.so`). Sealed memfds
  made into dma-bufs by `/dev/udmabuf`, as Android's CPU buffers are: LINEAR,
  256-byte row pitch, mappable, importable by llvmpipe and the compositor. RGB
  8888/565/888, 1010102 and YUV 4:2:0.
- **Build.** `build.sh` uses the NDK's clang for `x86_64-linux-musl`, musl's
  headers (a pinned release, headers only) and stand-ins that give the linker
  `libc.so`, `libc++.so` and `libhilog.so` sonames; the image's libraries
  resolve the symbols at load time. The C++ never includes libc++ headers: the
  few `std::vector` and `std::string` fields in the ABI are written in place
  (libc++'s `std::__h` layout), and the buffer VDI's vtable comes from declaring
  OH's interface in its order.
- **One window, not one per app.** OH composes every window into one screen
  buffer before the HDI sees it; layers reach the composer separately only when
  render_service offloads them to hardware composition, which it does for
  self-drawing surfaces (video, XComponent), not ordinary ArkUI windows.
  SceneBoard's window manager puts apps on screen 0, and OH's floating and
  freeform modes are SceneBoard's too. Per-app windows would need one OH screen
  per app window plus a window policy on the OH side that places each app on its
  own screen, so the bridge shows screen 0 as one phone-frame window.

## How it boots

The same path as Android's session ([docs/session.md](../docs/session.md)): a
root of symlinks in the sandbox's own `/`, then the broker, then init.

- **Root.** `/system` is the extracted partition, `/vendor` its symlinks with
  our VDIs in place of the image's, and `/sys_prod` is our layer. OH's product
  partition is empty in the image but on init's cfg path and the musl linker's
  search path, so our files go there and the image is never changed. The sandbox
  keeps its `/usr`, `/bin` and `/etc` for the glibc side; `/etc` gains OH's
  other entries and `/lib` gains the musl loader.
- **Init.** `/system/bin/init --second-stage`: the image's first stage only
  mounts block devices and switches root. The shim makes `getpid()` 1 in init
  (it refuses to run otherwise), makes mounts and namespace calls no-ops
  (Flatpak's seccomp filter refuses them before ours sees them) and priority
  raises best-effort. A mount namespace's `setns` still returns init to `/`, as
  the kernel does: init's `mksandbox` changes into its sandbox first, and
  services inherit init's cwd.
- **Apps.** appspawn gives each app a mount namespace: bind mounts into
  `/mnt/sandbox/<user>/<bundle>`, then `pivot_root` there, and apps find their
  own files only at those paths (`/data/storage/el1/bundle` is the app's code).
  In appspawn and the other spawners the shim makes those binds symlinks at
  their mount points, and `pivot_root` keeps the sandbox's `/data/storage` open
  on a fixed fd; the session's `/data/storage` links to `/proc/self/fd/<fd>`, so
  each app process sees its own. The rest of an app's root is the session's.
- **Inodes.** BMS checks free inodes before each install, and btrfs (no fixed
  inode table) reports none, which would refuse SceneBoard and the larger apps.
  The shim's `statfs` reports a free inode per free block there.
- **Broker.** The same static musl broker as Android's (OH's root has no
  Bionic), with `INNKEEPER_GUEST=ohos`: it also traps `open(2)` (musl doesn't
  use `openat`), serves `/etc/passwd` and `/etc/group` from `/system/etc`, adds
  OH's binder ioctls and serves `/dev/access_token_id`
  ([supervisor/README.md](../supervisor/README.md#openharmony)).
