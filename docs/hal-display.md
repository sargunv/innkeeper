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

- **YUV in the GPU drivers (gpu lane, 2026-10-27):** YV12, NV12 and NV21 buffers
  from this allocator import into ANGLE/RADV and lavapipe as Vulkan external
  formats with YCbCr conversion. Video frames composited by SurfaceFlinger
  render correctly; before Mesa patch 0005, YV12 aborted SurfaceFlinger. The
  drivers depend on four things from this HAL:
  - the fourcc (`PIXEL_FORMAT_FOURCC`);
  - per-plane offsets and strides in `PLANE_LAYOUTS`, with all planes in one
    dma-buf;
  - 256-byte pitches on every plane;
  - `DATASPACE` (Codec2 sets it; unset means BT.601 narrow range).

  Explicit YV12 stays YV12 and flexible YUV stays NV12: both render correctly,
  so producers need no steering. Details and tests are in
  [gpu.md](gpu.md#yuv-buffers-video-and-the-surfaceflinger-abort).

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
- **Virtual displays:** HWC-backed, up to 32 (`getMaxVirtualDisplayCount`), with
  ids from 100 that are never reused. The `.rc` sets `debug.sf.enable_hwc_vds=1`
  before SurfaceFlinger starts. Every layer stays CLIENT, so SurfaceFlinger
  renders each frame into the sink's buffer and passes that buffer as both
  client target and output buffer (`setOutputBuffer`). Virtual displays are
  powered on from the start and follow the size of their buffers
  (`VirtualDisplay.resize`). Each one shows in the Wayland window the window
  manager asked for, or stays headless. See [windowing.md](windowing.md).
- **Unsupported:** readback, content sampling, boot config, idle timer, HDR
  conversion, LUTs, sideband, EDID (SurfaceFlinger falls back to a port-based
  display id).
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

Set `vendor.linuxapk.hwc.backend=wayland` to have the composer itself show
displays as windows on the host compositor. The session default is `bridge`: the
same windows, but the host bridge's display service is the Wayland client
(`composer/BridgePresenter.cpp`, [windowing.md](windowing.md), and
[bridge.md](bridge.md)); the composer falls back to `wayland` if the bridge is
unreachable. Either way:

- **Per-app mode** (default, `vendor.linuxapk.wm.mode` unset or `per-app`):
  display 0 stays headless, and each virtual display the window manager requests
  gets its own `xdg_toplevel` ([windowing.md](windowing.md)).
- **Single-window mode** (`vendor.linuxapk.wm.mode=single`, set by
  `linuxapk-session start --single-window`): display 0 is the "Linux.apk"
  window, as in M1.

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
- **Windows:** one `xdg_toplevel` per shown display, with server-side
  decorations via xdg-decoration. The window registry keys windows by Android
  display id:
  - display 0 is titled "Linux.apk" with app id `linuxapk`;
  - app windows take the title and app id from the window manager's request.

  Pointer, keyboard and touch focus are tracked per `wl_surface`, so events
  carry the id of the window they belong to. `xdg_activation_v1` is used for
  `window_activate`.

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
- **Scaling:** `wp_viewporter` stretches the newest frame over the window's
  logical size from `xdg_toplevel.configure`.
  - Display 0 keeps a fixed size; its window starts at the display size in
    logical pixels, and the aspect ratio isn't preserved when the user resizes.
  - A virtual display's window starts at its size in dp (pixels × 160 / dpi).
    The window manager then resizes the display to the window's logical size
    times the fractional scale, and the composer swaps its buffer pool when
    frames of a new size arrive.
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

### Input

The composer sees no input. The host seat reaches the Linux.apk System app
through the bridge's input service (`host/bridge/src/input/proto.rs`, which
lists every input line; [bridge.md](bridge.md#input-service-id-6)).

## Tests

| What                                                                                                                                                                                                               | Command                                | Evidence                                                                                                                                              |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host unit tests (layouts, every format, 256-byte plane pitches, udmabuf alloc and the no-udmabuf policy, mapper via `dlopen`, metadata get/set across imports and `fork`, sync_file acquire fence, PNG/PPM writer) | `hal/build.sh` (ctest)                 | `260 + 8 checks, 0 failures`                                                                                                                          |
| On-device gralloc                                                                                                                                                                                                  | `hal/build.sh` (`tests/bionic/run.sh`) | IAllocator V3, 10 formats allocated as udmabufs, plus `AHardwareBuffer_allocate/lock/lockPlanes` through libui: `110 checks, 0 failures`              |
| On-device composer                                                                                                                                                                                                 | `hal/build.sh` (`tests/bionic/run.sh`) | hotplug, 60 vsyncs/s on a 16.667 ms grid, DEVICE→CLIENT, present, NOT_VALIDATED, slot cache, PNG in `out/test/bionic/frames`: `55 checks, 0 failures` |

`tests/bionic/run.sh` needs no full session. It runs inside a transient
`systemd-run --user` unit, in a user+mount+pid namespace, with:

- the session image's `/system`, `/system_ext`, `/product` and flattened
  `/apex`;
- `/vendor` = `out/vendor-overlay` plus the test binaries;
- binder from the broker, behind the session's pivot-exec;
- the image's `servicemanager` and our two services.

`tests/bionic/props-shim.c` (LD_PRELOAD, test only) stands in for the missing
pieces. It fakes the selinux status, fakes properties (seeding
`servicemanager.ready`, plus `LINUXAPK_PROPS` via `-e`) and mirrors liblog to
stderr.

### Wayland proof (2026-10-27, KDE Plasma 6 / KWin on this machine)

```sh
# The old hal/tests/run-android.sh; its -w mode is gone.
hal/tests/run-android.sh -w -e VENDOR_LINUXAPK_HWC_BACKEND=wayland \
  -e HWC_TEST_ANIMATE_SEC=25 -- /vendor/bin/hwc_test
```

`-w` bound only the host's Wayland socket file at `/dev/socket/wayland-host`.
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
