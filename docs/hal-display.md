# Display HALs: gralloc and composer

Our vendor graphics HALs, built out of tree with the NDK (no AOSP build).
Sources are in `hal/`.

| Piece | Interface (Android 17 FCM 202604) | Installed as |
|---|---|---|
| Allocator | `android.hardware.graphics.allocator` AIDL **V3**, `IAllocator/default` | `bin/hw/android.hardware.graphics.allocator-service.linuxapk` |
| Mapper | stable-C IMapper **v5**, VINTF `<hal format="native">mapper` 5.0, instance `linuxapk` | `lib64/hw/mapper.linuxapk.so` |
| Composer | `android.hardware.graphics.composer3` AIDL **V5**, `IComposer/default` | `bin/hw/android.hardware.graphics.composer3-service.linuxapk` |

These are the versions the emulator image's own HALs ship. The matrix allows
allocator 1-3 and composer3 4-5. libui's Gralloc5 needs allocator ≥ 2, gets
the mapper suffix from `getIMapperLibrarySuffix()` (`"linuxapk"`), and
loads `mapper.linuxapk.so` via `AServiceManager_openDeclaredPassthroughHal`.
That call needs the VINTF fragment. Init `.rc` files and VINTF fragments are
in `hal/etc/`.

## Build

```sh
hal/build.sh            # host unit tests, Android build, install
hal/build.sh --no-test
```

The script writes to `out/hal-build/` and installs into `out/vendor-overlay/`
(`bin/hw`, `lib64/hw`, `etc/init`, `etc/vintf/manifest`). Test binaries go to
`out/hal-build/tests/`. It needs cmake, ninja, python3 and clang; if any is
missing it re-runs itself in `tools/hosttools.sh`.

- **Headers and AIDL:** `hal/third_party/aosp/` holds the frozen AIDL
  snapshots (allocator 3, composer3 5, graphics.common 7, common 2,
  drm.common 1) plus the binder NDK and IMapper headers, copied from
  android-17.0.0_r1 by `hal/tools/sync-aosp.sh`. `hal/tools/gen-aidl.sh` runs
  `aidl --lang=ndk` with the frozen version and hash.
- **Linking:** we may link only LL-NDK libraries. `hal/stubs/gen_stub.py`
  turns AOSP's `libbinder_ndk.map.txt` into a link-only stub, so the platform
  symbols `AServiceManager_addService` and `AIBinder_markVintfStability`
  resolve. libc++ is linked statically, and zlib statically from the NDK.
  The mapper exports only `AIMapper_loadIMapper` and
  `ANDROID_HAL_STABLEC_VERSION`.
- **Dependencies:** binaries need only libc/libm/libdl/liblog/libbinder_ndk.
  The mapper needs libc/libm/libdl/liblog. There's no libcutils, libui or
  libbase; `common/` reimplements the `native_handle` helpers.

## Buffers (`hal/common`)

- **Allocation:** each buffer is a sealed memfd turned into a dma-buf with
  `UDMABUF_CREATE`. Everything is linear and CPU-mappable; RADV imports these
  as GTT memory (docs/gpu.md), and linux-dmabuf can take fd 0 directly.
  Without `/dev/udmabuf`, or above `udmabuf.size_limit_mb` (64 MiB), only
  CPU-only usages fall back to a plain memfd. GPU and composer usages (any
  `GPU_*`, `COMPOSER_*`) fail with UNSUPPORTED or NO_RESOURCES, because
  their consumers can't import a memfd. The session must bind
  `/dev/udmabuf`. Only the session's root (the host user, who holds the
  logind ACL) can open it, so the allocator's `.rc` runs it as root. It opens
  the device first and then drops to `system:graphics`. This replaces the
  session lane's interim `user root` override.
- **Handle** (`gralloc_handle.h`): 1 fd + ints, holding size, format, DRM
  fourcc, usage, buffer id, and per-plane offset and stride.
- **fd layout:** `[pixels, layerCount layers][4 KiB SharedMetadata][reserved region]`.
  Mutable metadata (dataspace, blend mode, SMPTE2086, CTA861.3, SMPTE2094-10,
  -40 and -50 blobs) lives in the buffer itself, so every importing process
  sees the same values.
- **Rows:** every plane's row pitch is a multiple of 256 bytes, which RADV
  needs for linear images. YV12 keeps Android's rule
  (`c_stride = align(y_stride/2, 16)`) by padding luma to 512 bytes. BLOB
  rows aren't padded.
- **Formats:**

  | Android format | DRM fourcc |
  |---|---|
  | RGBA_8888 | AB24 |
  | RGBX_8888 | XB24 |
  | BGRA_8888 | AR24 |
  | RGB_888 | BG24 |
  | RGB_565 | RG16 |
  | RGBA_FP16 | AB4H |
  | RGBA_1010102 | AB30 |
  | R_8, BLOB, Y8 | R8 |
  | R_16_UINT, Y16, RAW16 | R16 |
  | RG_1616_UINT | GR32 |
  | YCBCR_420_888 (allocated as NV12) | NV12 |
  | YCRCB_420_SP | NV21 |
  | YV12 (Android's layout: Cr then Cb, `c_stride = align(y_stride/2, 16)`) | YV12 |
  | YCBCR_P010 | P010 |

  IMPLEMENTATION_DEFINED becomes NV12 for camera and video usage, and RGBA
  otherwise. PROTECTED usage is unsupported.
- **Mapper:** all standard metadata types up to SMPTE2094_50 are gettable,
  using AOSP's `IMapperMetadataTypes.h` encoders. `lock` waits on the acquire
  fence and closes it, then returns the base pointer. `unlock` returns -1.
  There's no `DMA_BUF_IOCTL_SYNC`, because x86 shmem is coherent.

## Composer (`hal/composer`)

- **Display:** one display, id 0, 1280x800 @ 60 Hz at 160 dpi by default. It
  reads the properties `vendor.linuxapk.display.{width,height,refresh_hz,dpi}`
  at service start; if a property isn't set, the matching env var is used,
  e.g. `VENDOR_LINUXAPK_DISPLAY_WIDTH`.
- **Vsync:** a CLOCK_MONOTONIC `clock_nanosleep` thread on an exact grid
  calls `onVsync` while it's enabled. On registration the composer sends
  `onHotplugEvent(CONNECTED)`.
- **Composition:** `validate` turns every non-CLIENT layer into CLIENT and
  reports it in `ChangedCompositionTypes`. SurfaceFlinger's RenderEngine then
  composites into the client target. `presentOrValidate` always validates.
- **Fences:** capability `PRESENT_FENCE_IS_NOT_RELIABLE`. Present returns no
  present fence and no release fences (both mean "already signalled"),
  because the headless backend holds no buffer after `present` returns.
- **Buffers:** buffer slots are cached per layer and for the client target,
  and honour `bufferSlotsToClear`. Each `BufferRef` is reference-counted, so
  a backend can keep a buffer alive past slot replacement.
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
  `/data/vendor/linuxapk/frames`. The properties are re-read every second,
  so `setprop vendor.linuxapk.hwc.dump.dir /data/vendor/linuxapk/frames`
  works on a running session.

### Adding the Wayland backend (M1)

`Presenter.h` is the seam. `Display` owns all HWC3 state and asks the
presenter two things:

1. `acceptDeviceLayer(layer)` during validate: return true to keep a layer as
   DEVICE.
2. `present(FrameInput&, PresentResult*)` with the client target and the
   z-sorted layers. Each layer carries its `BufferRef`, acquire fence, frame,
   crop, transform and alpha.

`createPresenter()` picks the backend from `vendor.linuxapk.hwc.backend`. A
`WaylandPresenter` would work like this:

- Start with one `wl_surface` for the client target. Wrap the dma-buf from
  `BufferRef::info()` with `zwp_linux_dmabuf_v1` (fourcc and offsets/strides
  from the handle, modifier LINEAR), or copy into `wl_shm` when the
  compositor rejects it.
- Then accept DEVICE layers per task and map each to its own surface or
  subsurface.
- We can't create sync_files (no sw_sync for this user). So, until
  `wl_buffer.release`, either keep the `BufferRef` and copy, or keep
  returning no release fences and only accept buffers that SurfaceFlinger
  cycles through 3+ slots.
- Vsync could come from `wp_presentation` feedback instead of the timer.

## Tests

| What | Command | Evidence |
|---|---|---|
| Host unit tests (layouts, every format, 256-byte plane pitches, udmabuf alloc and the no-udmabuf policy, mapper via `dlopen`, metadata get/set across imports and `fork`, sync_file acquire fence, PNG/PPM writer) | `hal/build.sh` (ctest) | `260 + 8 checks, 0 failures` |
| On-device gralloc | `hal/tests/run-android.sh -- /vendor/bin/gralloc_test` | IAllocator V3, 10 formats allocated as udmabufs, plus `AHardwareBuffer_allocate/lock/lockPlanes` through libui: `110 checks, 0 failures` |
| On-device composer | `hal/tests/run-android.sh -e VENDOR_LINUXAPK_HWC_DUMP_DIR=/data/vendor/linuxapk/frames -e EXPECT_DUMP=/data/vendor/linuxapk/frames -- /vendor/bin/hwc_test` | hotplug, 60 vsyncs/s on a 16.667 ms grid, DEVICE→CLIENT, present, NOT_VALIDATED, slot cache, PNG in `out/hal-test/frames`: `55 checks, 0 failures` |

`hal/tests/run-android.sh` needs no full session. It runs inside a transient
`systemd-run --user` unit, in a user+mount+pid namespace, with:

- the image's `/system`, `/system_ext`, `/product` and flattened `/apex`;
- `/vendor` = `out/vendor-overlay` plus the test binaries;
- a private binderfs, and no `/sys`;
- the image's `servicemanager` and our two services.

`tests/android/haltest_shim.c` (LD_PRELOAD, test only) stands in for the
missing pieces. It fakes the selinux status, fakes properties (seeding
`servicemanager.ready`, plus `LINUXAPK_PROPS` via `-e`) and mirrors liblog to
stderr.
