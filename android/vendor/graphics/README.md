# graphics

This directory holds the display HALs. `mise run build:ndk`, the NDK superbuild,
builds them out of tree and stages them under `share/innkeeper/android/vendor/`.
init starts the allocator and composer services from `etc/init`. Every process
that touches gralloc buffers loads the mapper.

| Piece     | Interface                                                           | Installed as                                                   |
| --------- | ------------------------------------------------------------------- | -------------------------------------------------------------- |
| allocator | `android.hardware.graphics.allocator` AIDL V3, `IAllocator/default` | `bin/hw/android.hardware.graphics.allocator-service.innkeeper` |
| mapper    | stable-C IMapper v5, instance `innkeeper`                           | `lib64/hw/mapper.innkeeper.so`                                 |
| composer  | `android.hardware.graphics.composer3` AIDL V5, `IComposer/default`  | `bin/hw/android.hardware.graphics.composer3-service.innkeeper` |

- **Buffers** (`common/`, `allocator/gpu_buffer.cpp`). For GPU-only RGB render
  and composer targets, the allocator uses the session's selected Vulkan driver
  to choose a single-plane tiled layout and exports the allocation as a dma-buf.
  Buffers with CPU access, cursor buffers, YUV buffers and unsupported
  allocations use a sealed memfd and `/dev/udmabuf`. The native handle has two
  fds. The first holds the pixels. The second is a sealed memfd with a 4 KiB
  shared metadata page, followed by any client reserved region. The metadata
  page carries the DRM modifier, the dataspace and HDR metadata. Keeping the
  metadata separate lets a driver's dedicated allocation hold the pixels alone.
  Compression with auxiliary planes is excluded.
  [docs/display.md](../../../docs/display.md#buffers) gives the pitch and YUV
  rules. Only the session's root may open `/dev/udmabuf`, so the allocator opens
  it before it drops to `system:graphics`.
- **Composer.** The composer has display 0 plus up to 32 HWC-backed virtual
  displays. Display 0 is headless at 1280x800, except in
  [phone mode](../../../docs/display.md#phone-mode). Content layers are CLIENT.
  CURSOR layers go to the Wayland cursor. `vendor.innkeeper.hwc.backend` picks
  the presenter (`Presenter.cpp`). With `bridge`, which the launcher sets for
  normal sessions, virtual displays go to the bridge's display service. With
  `headless`, nothing is shown.
- **Vsync.** Display 0 supplies Android's scheduling clock. It follows the
  refresh rate and `wp_presentation` phase of the output that shows a visible
  mapped window. It keeps that phase while the window is static. Without such a
  window, it falls back to a 60 Hz timer. The rc sets the app and SurfaceFlinger
  work durations to 4 ms each, to fit a 120 Hz frame.
- **LL-NDK only.** The binder NDK and IMapper headers and the frozen AIDL
  snapshots come from `android/vendor/third_party/aosp`. `gen-stub.py` (in
  `android/vendor/tools`) turns AOSP's `libbinder_ndk.map.txt` into a link-only
  stub for the platform symbols, and every HAL links that stub. GPU allocation
  uses the LL-NDK libvulkan. The native handle and the usage policy stay in our
  allocator, so there is no GBM or minigbm allocator and no other gralloc HAL.

Tests: `tests/host` runs in `mise run test`. It covers layouts, every format,
the mapper through `dlopen`, and metadata across imports and `fork`.
`tests/android` (`gralloc_test`, `hwc_test`) runs in `mise run test:bionic`, in
a session with the framework stopped. Start reading at `common/src/layout.cpp`,
`composer/Display.cpp` and `composer/BridgePresenter.cpp`.
