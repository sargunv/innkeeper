# Display HALs: gralloc and composer

Our vendor graphics HALs, built out of tree with the NDK (no AOSP build).
Sources are in `hal/`.

| Piece     | Interface (Android 17 FCM 202604)                                                     | Installed as                                                  |
| --------- | ------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
| Allocator | `android.hardware.graphics.allocator` AIDL **V3**, `IAllocator/default`               | `bin/hw/android.hardware.graphics.allocator-service.linuxapk` |
| Mapper    | stable-C IMapper **v5**, VINTF `<hal format="native">mapper` 5.0, instance `linuxapk` | `lib64/hw/mapper.linuxapk.so`                                 |
| Composer  | `android.hardware.graphics.composer3` AIDL **V5**, `IComposer/default`                | `bin/hw/android.hardware.graphics.composer3-service.linuxapk` |

These are the versions the emulator image's own HALs ship. The matrix allows
allocator 1-3 and composer3 4-5. libui's Gralloc5 needs allocator ≥ 2, gets the
mapper suffix from `getIMapperLibrarySuffix()` (`"linuxapk"`), and loads
`mapper.linuxapk.so` via `AServiceManager_openDeclaredPassthroughHal`. That call
needs the VINTF fragment. Init `.rc` files and VINTF fragments are in
`hal/etc/`.

## Build

```sh
hal/build.sh            # host unit tests, Android build, install
hal/build.sh --no-test
```

The script writes to `out/hal-build/` and installs into `out/vendor-overlay/`
(`bin/hw`, `lib64/hw`, `etc/init`, `etc/vintf/manifest`). Test binaries go to
`out/hal-build/tests/`. It needs cmake, ninja, python3 and clang; if any is
missing it re-runs itself in `tools/hosttools.sh`.

- **Headers and AIDL:** `hal/third_party/aosp/` holds the frozen AIDL snapshots
  (allocator 3, composer3 5, graphics.common 7, common 2, drm.common 1) plus the
  binder NDK and IMapper headers, copied from android-17.0.0_r1 by
  `hal/tools/sync-aosp.sh`. `hal/tools/gen-aidl.sh` runs `aidl --lang=ndk` with
  the frozen version and hash.
- **Linking:** we may link only LL-NDK libraries. `hal/stubs/gen_stub.py` turns
  AOSP's `libbinder_ndk.map.txt` into a link-only stub, so the platform symbols
  `AServiceManager_addService` and `AIBinder_markVintfStability` resolve. libc++
  is linked statically, and zlib statically from the NDK. The mapper exports
  only `AIMapper_loadIMapper` and `ANDROID_HAL_STABLEC_VERSION`.
- **Dependencies:** binaries need only libc/libm/libdl/liblog/libbinder_ndk. The
  mapper needs libc/libm/libdl/liblog. There's no libcutils, libui or libbase;
  `common/` reimplements the `native_handle` helpers.

## Buffers (`hal/common`)

- **Allocation:** each buffer is a sealed memfd turned into a dma-buf with
  `UDMABUF_CREATE`. Everything is linear and CPU-mappable; RADV imports these as
  GTT memory (docs/gpu.md), and linux-dmabuf can take fd 0 directly. Without
  `/dev/udmabuf`, or above `udmabuf.size_limit_mb` (64 MiB), only CPU-only
  usages fall back to a plain memfd. GPU and composer usages (any `GPU_*`,
  `COMPOSER_*`) fail with UNSUPPORTED or NO_RESOURCES, because their consumers
  can't import a memfd. The session must bind `/dev/udmabuf`. Only the session's
  root (the host user, who holds the logind ACL) can open it, so the allocator's
  `.rc` runs it as root. It opens the device first and then drops to
  `system:graphics`. This replaces the session lane's interim `user root`
  override.
- **Handle** (`gralloc_handle.h`): 1 fd + ints, holding size, format, DRM
  fourcc, usage, buffer id, and per-plane offset and stride.
- **fd layout:**
  `[pixels, layerCount layers][4 KiB SharedMetadata][reserved region]`. Mutable
  metadata (dataspace, blend mode, SMPTE2086, CTA861.3, SMPTE2094-10, -40 and
  -50 blobs) lives in the buffer itself, so every importing process sees the
  same values.
- **Rows:** every plane's row pitch is a multiple of 256 bytes, which RADV needs
  for linear images. YV12 keeps Android's rule
  (`c_stride = align(y_stride/2, 16)`) by padding luma to 512 bytes. BLOB rows
  aren't padded.
- **Formats:**

  | Android format                                                          | DRM fourcc |
  | ----------------------------------------------------------------------- | ---------- |
  | RGBA_8888                                                               | AB24       |
  | RGBX_8888                                                               | XB24       |
  | BGRA_8888                                                               | AR24       |
  | RGB_888                                                                 | BG24       |
  | RGB_565                                                                 | RG16       |
  | RGBA_FP16                                                               | AB4H       |
  | RGBA_1010102                                                            | AB30       |
  | R_8, BLOB, Y8                                                           | R8         |
  | R_16_UINT, Y16, RAW16                                                   | R16        |
  | RG_1616_UINT                                                            | GR32       |
  | YCBCR_420_888 (allocated as NV12)                                       | NV12       |
  | YCRCB_420_SP                                                            | NV21       |
  | YV12 (Android's layout: Cr then Cb, `c_stride = align(y_stride/2, 16)`) | YV12       |
  | YCBCR_P010                                                              | P010       |

  IMPLEMENTATION_DEFINED becomes NV12 for camera and video usage, and RGBA
  otherwise. PROTECTED usage is unsupported.

- **Mapper:** all standard metadata types up to SMPTE2094_50 are gettable, using
  AOSP's `IMapperMetadataTypes.h` encoders. `lock` waits on the acquire fence
  and closes it, then returns the base pointer. `unlock` returns -1. There's no
  `DMA_BUF_IOCTL_SYNC`, because x86 shmem is coherent.

## Composer (`hal/composer`)

- **Display:** one display, id 0, 1280x800 @ 60 Hz at 160 dpi by default. It
  reads the properties `vendor.linuxapk.display.{width,height,refresh_hz,dpi}`
  at service start; if a property isn't set, the matching env var is used, e.g.
  `VENDOR_LINUXAPK_DISPLAY_WIDTH`.
- **Vsync:** a CLOCK_MONOTONIC `clock_nanosleep` thread on an exact grid calls
  `onVsync` while it's enabled. On registration the composer sends
  `onHotplugEvent(CONNECTED)`.
- **Composition:** `validate` turns every non-CLIENT layer into CLIENT and
  reports it in `ChangedCompositionTypes`. SurfaceFlinger's RenderEngine then
  composites into the client target. `presentOrValidate` always validates.
- **Fences:** capability `PRESENT_FENCE_IS_NOT_RELIABLE`. Present returns no
  present fence and no release fences (both mean "already signalled"), because
  the headless backend holds no buffer after `present` returns.
- **Buffers:** buffer slots are cached per layer and for the client target, and
  honour `bufferSlotsToClear`. Each `BufferRef` is reference-counted, so a
  backend can keep a buffer alive past slot replacement.
- **Unsupported:** virtual displays (SurfaceFlinger does them with the GPU),
  readback, content sampling, boot config, idle timer, HDR conversion, LUTs,
  sideband, EDID (SurfaceFlinger falls back to a port-based display id).
- **Frame dumps:** set `vendor.linuxapk.hwc.dump.dir` (or the env var
  `VENDOR_LINUXAPK_HWC_DUMP_DIR`) and the composer writes
  `<dir>/display0-latest.png` atomically, at most once per
  `vendor.linuxapk.hwc.dump.interval_ms` (default 1000). Related settings:
  - `vendor.linuxapk.hwc.dump.format`: `png` or `ppm`.
  - `vendor.linuxapk.hwc.dump.keep=N`: also rotate N numbered files.

  `present` only waits on the acquire fence and copies the pixels; a worker
  thread does the encoding. The composer's `.rc` creates
  `/data/vendor/linuxapk/frames`. The properties are re-read every second, so
  `setprop vendor.linuxapk.hwc.dump.dir /data/vendor/linuxapk/frames` works on a
  running session.

### Presenter backends

`Presenter.h` is the seam. `Display` owns all HWC3 state and asks the presenter
two things:

1. `acceptDeviceLayer(layer)` during validate: return true to keep a layer as
   DEVICE.
2. `present(FrameInput&, PresentResult*)` with the client target and the
   z-sorted layers. Each layer carries its `BufferRef`, acquire fence, frame,
   crop, transform and alpha.

`createPresenter()` picks the backend from `vendor.linuxapk.hwc.backend`:
`headless` (default) or `wayland`. If the Wayland backend can't start, it falls
back to headless. In Wayland mode the headless presenter still runs alongside,
so frame dumps keep working.

## Wayland backend (`composer/WaylandPresenter.cpp`, M1)

Set `vendor.linuxapk.hwc.backend=wayland` to show display 0 as a window on the
host compositor.

- **Dependencies:** `hal/wayland/fetch-deps.sh` (run by `build.sh`) fetches and
  sha256-checks three pinned sources:
  - wayland 1.24.0, for libwayland-client;
  - wayland-protocols 1.45, for xdg-shell, viewporter, presentation-time,
    linux-dmabuf v1, fractional-scale-v1 and xdg-decoration;
  - libffi 3.5.2, which it cross-compiles with the NDK.

  Everything is linked statically into the composer, whose dependencies stay
  LL-NDK only. The protocol code is generated with the host's `wayland-scanner`
  (version 1.22 or newer).

- **Connection:** the composer starts as root. Before dropping to
  `system:graphics`, it connects to `vendor.linuxapk.wayland.socket` (default
  `/dev/socket/wayland-host`, where the session binds the host's
  `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY`). It has to connect as root, because only
  the session's root (the host user) may connect to the host socket. The
  connection is a process singleton with its own event thread. Each composer
  client (each SurfaceFlinger instance) gets its own window.
- **Window:** one `xdg_toplevel` titled "Linux.apk" (app id `linuxapk`), with
  server-side decorations via xdg-decoration.
- **Buffers:** the composer copies each client target (RGBA/RGBX/BGRA 8888) into
  a pool of 3 of its own buffers, so SurfaceFlinger's buffer is free again as
  soon as `present` returns (still no fences). The pool is one of:
  - linear udmabufs (`XBGR8888`, modifier LINEAR), imported with
    `zwp_linux_dmabuf_v1` v3. They're created with the async `create`, so a
    rejection falls back instead of killing the connection.
  - `wl_shm`, either as a fallback or forced with
    `vendor.linuxapk.wayland.force_shm=1`. It uses `XBGR8888` if offered, else
    the mandatory `XRGB8888` with an R/B swizzle; KWin only offers `XRGB8888`.

  The composer never writes into a buffer until its `wl_buffer.release` arrives.
  If all 3 are held, it drops the frame and counts it.

- **Pacing:** vsync stays on the composer's timer. `wp_presentation` feedback
  only counts shown and discarded frames, logged every 10 s.
- **Scaling:** the Android display size stays fixed (M1). `wp_viewporter`
  stretches the buffer over the window's logical size from
  `xdg_toplevel.configure`. The default window is the display size in logical
  pixels, so the compositor applies the fractional scale. The aspect ratio isn't
  preserved when the user resizes.
- **Real resize (later):**
  1. On configure, compute a new mode: the window's logical size × the
     fractional scale (`wp_fractional_scale_v1`), rounded to whole pixels.
  2. Report it as a new `DisplayConfiguration` and let SurfaceFlinger switch.
     Or, since the HWC3 mode list is static, hotplug the display again with the
     new single mode, which is how emulator HWCs resize.
  3. Map the scale to density through the desktop bridge.
  4. Keep the viewport at 1:1 until SurfaceFlinger's next client target arrives
     at the new size.
- **Next steps:**
  - zero-copy: attach the client target's own dma-buf and hold its `BufferRef`
    until release;
  - per-layer subsurfaces (DEVICE layers);
  - frame callbacks driving vsync;
  - formats other than 8888.

### Input event stream

The composer listens on `vendor.linuxapk.input.socket` (default
`/dev/socket/linuxapk-input`, `AF_UNIX` `SOCK_STREAM`). If init created a socket
of that name, the composer uses init's socket (`$ANDROID_SOCKET_<name>`) instead
of binding the path itself.

Each connected client receives the same stream of events from the window's
`wl_seat`. Every event is one ASCII line of space-separated fields ending in
`\n`:

- Coordinates are in **display pixels**: surface-local position × display size /
  window size, as floats with 2 decimals.
- `<t>` is the compositor's millisecond timestamp.
- Button and key codes are Linux evdev codes (`BTN_LEFT`=272, `KEY_A`=30).
- A client that falls behind (its socket buffer fills up) is disconnected.

| Line                                                                                  | When                                                              |
| ------------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `hello 1 <display_w> <display_h>`                                                     | first line on connect (protocol version 1)                        |
| `window <logical_w> <logical_h> <scale>`                                              | on connect and whenever the window size or scale changes          |
| `pointer_enter <x> <y>` / `pointer_leave`                                             | pointer enters or leaves the window                               |
| `pointer_motion <t> <x> <y>`                                                          | pointer moves                                                     |
| `pointer_button <t> <button> <0\|1>`                                                  | button released (0) or pressed (1)                                |
| `pointer_axis <t> <axis> <value>`                                                     | scroll, axis 0 = vertical, 1 = horizontal; value in surface units |
| `pointer_axis_value120 <axis> <v120>`                                                 | high-resolution wheel steps (120 = one detent)                    |
| `pointer_axis_stop <t> <axis>`                                                        | end of a kinetic scroll                                           |
| `pointer_frame`                                                                       | ends a group of pointer events                                    |
| `keyboard_enter` / `keyboard_leave`                                                   | keyboard focus                                                    |
| `key <t> <keycode> <0\|1>`                                                            | key released (0) or pressed (1)                                   |
| `modifiers <depressed> <latched> <locked> <group>`                                    | xkb masks (compositor keymap)                                     |
| `touch_down <t> <id> <x> <y>` / `touch_motion <t> <id> <x> <y>` / `touch_up <t> <id>` | touch points                                                      |
| `touch_frame` / `touch_cancel`                                                        | end of a touch group, or the touch sequence was cancelled         |
| `close`                                                                               | the user asked to close the window                                |

Example: `socat - UNIX-CONNECT:/dev/socket/linuxapk-input`.

## Tests

| What                                                                                                                                                                                                               | Command                                                                                                                                                     | Evidence                                                                                                                                           |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host unit tests (layouts, every format, 256-byte plane pitches, udmabuf alloc and the no-udmabuf policy, mapper via `dlopen`, metadata get/set across imports and `fork`, sync_file acquire fence, PNG/PPM writer) | `hal/build.sh` (ctest)                                                                                                                                      | `260 + 8 checks, 0 failures`                                                                                                                       |
| On-device gralloc                                                                                                                                                                                                  | `hal/tests/run-android.sh -- /vendor/bin/gralloc_test`                                                                                                      | IAllocator V3, 10 formats allocated as udmabufs, plus `AHardwareBuffer_allocate/lock/lockPlanes` through libui: `110 checks, 0 failures`           |
| On-device composer                                                                                                                                                                                                 | `hal/tests/run-android.sh -e VENDOR_LINUXAPK_HWC_DUMP_DIR=/data/vendor/linuxapk/frames -e EXPECT_DUMP=/data/vendor/linuxapk/frames -- /vendor/bin/hwc_test` | hotplug, 60 vsyncs/s on a 16.667 ms grid, DEVICE→CLIENT, present, NOT_VALIDATED, slot cache, PNG in `out/hal-test/frames`: `55 checks, 0 failures` |

`hal/tests/run-android.sh` needs no full session. It runs inside a transient
`systemd-run --user` unit, in a user+mount+pid namespace, with:

- the image's `/system`, `/system_ext`, `/product` and flattened `/apex`;
- `/vendor` = `out/vendor-overlay` plus the test binaries;
- a private binderfs, and no `/sys`;
- the image's `servicemanager` and our two services.

`tests/android/haltest_shim.c` (LD_PRELOAD, test only) stands in for the missing
pieces. It fakes the selinux status, fakes properties (seeding
`servicemanager.ready`, plus `LINUXAPK_PROPS` via `-e`) and mirrors liblog to
stderr.

### Wayland proof (2026-10-27, KDE Plasma 6 / KWin on this machine)

```sh
hal/tests/run-android.sh -w -e VENDOR_LINUXAPK_HWC_BACKEND=wayland \
  -e HWC_TEST_ANIMATE_SEC=25 -- /vendor/bin/hwc_test
```

`-w` binds only the host's Wayland socket file at `/dev/socket/wayland-host`.
`HWC_TEST_ANIMATE_SEC` keeps `hwc_test` presenting a blinking test pattern for
that many seconds and prints the input stream. Results:

- A "Linux.apk" window appeared with KWin decorations and the gradient and
  checkerboard pattern. The screenshot is in `out/hal-test/wayland-window.png`
  (taken with `spectacle -b -n`, cropped to the window).
- Log line:
  `wayland: connected ... dmabuf=v3 (XBGR8888 linear: yes), viewporter=yes, fractional-scale=yes, xdg-decoration=yes, presentation=yes, seat=yes`.
- Over 528 frames: 527 shown by the compositor (presentation-time), 2 discarded,
  0 dropped, window 1280x800 at scale 1.70, refresh 120 Hz.
- The input stream delivered `hello 1 1280 800`, `window 1280 800 1.700`,
  `pointer_motion <t> 501.49 564.17` and so on, plus `pointer_frame` and
  `keyboard_leave`, while the mouse moved over the window.
- With `VENDOR_LINUXAPK_WAYLAND_FORCE_SHM=1`, the `wl_shm` (XRGB8888) path
  showed 258 of 258 frames.
- `hwc_test` passed all its checks in both runs.

## Status in the live session (2026-10-27)

What works in the session lane's boot, with the session's interim `user root`
allocator override:

- init starts both services.
- SurfaceFlinger connects to the composer and reports display 0 at 1280x800 @ 60
  Hz, 160 dpi.
- SurfaceFlinger allocates its client targets (`LegacyFramebufferSurface`, usage
  0x1a00) and RenderEngine buffers through our allocator.
- RenderEngine runs ANGLE on RADV, which uses `mapper.linuxapk.so`.

No frame has been composed yet, because there are no layers: system_server isn't
up. I ran `bootanimation` by hand to get a layer. Its buffer was allocated
(`[BBQ] BootAnimation`, 1280x800, usage 0xb00), but the process then crashed in
RADV's `vkAcquireNextImageKHR`. That's in the gpu lane's driver, not in the
gralloc or the composer.
