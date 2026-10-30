# graphics

The display HALs, built out of tree with the NDK by `mise run build:graphics`
(the NDK superbuild) and staged under `share/linuxapk/android/vendor/`. init
starts the two services from `etc/init`; the mapper is loaded into every process
that touches gralloc buffers.

| Piece     | Interface                                                           | Installed as                                                  |
| --------- | ------------------------------------------------------------------- | ------------------------------------------------------------- |
| allocator | `android.hardware.graphics.allocator` AIDL V3, `IAllocator/default` | `bin/hw/android.hardware.graphics.allocator-service.linuxapk` |
| mapper    | stable-C IMapper v5, instance `linuxapk`                            | `lib64/hw/mapper.linuxapk.so`                                 |
| composer  | `android.hardware.graphics.composer3` AIDL V5, `IComposer/default`  | `bin/hw/android.hardware.graphics.composer3-service.linuxapk` |

- **Buffers** (`common/`): a sealed memfd turned into a dma-buf through
  `/dev/udmabuf`; the fd holds the pixels, then a 4 KiB shared metadata block,
  so every importer sees the same dataspace and HDR metadata. Pitch and YUV
  rules are in [docs/display.md](../../../docs/display.md#buffers). Only the
  session's root may open `/dev/udmabuf`, so the allocator opens it before
  dropping to `system:graphics`.
- **Composer:** one headless display 0 (1280x800) plus up to 32 HWC-backed
  virtual displays; content layers are CLIENT and CURSOR layers are exported
  separately to Wayland. `Presenter.cpp` picks the backend from
  `vendor.linuxapk.hwc.backend`: `bridge` (the session default) sends virtual
  displays to the bridge's display service, `headless` shows nothing.
  `vendor.linuxapk.hwc.dump.dir` writes PNG frame dumps. Display 0 supplies
  Android's scheduling clock for the virtual displays. It follows a visible
  mapped window's output refresh and `wp_presentation` phase, retaining that
  phase while the window is static. Visibility, surface enter/leave and teardown
  select the source; without a visible mapped source it uses a 60 Hz timer. The
  output mode seeds the period before the first feedback. Startup rc sets app/SF
  work durations to 4/4 ms, fitting within a 120 Hz frame. Set
  `vendor.linuxapk.hwc.host_clock=0` to compare against the fixed timer.
- **LL-NDK only.** `third_party/aosp` holds the frozen AIDL snapshots and binder
  NDK and IMapper headers (`tools/sync-aosp.sh`); `tools/gen-aidl.sh` generates
  the NDK backend with the build-tools' `aidl`, and `tools/gen-stub.py` turns
  AOSP's `libbinder_ndk.map.txt` into a link-only stub for the platform symbols.
  Binaries need only libc, libm, libdl, liblog and libbinder_ndk.

Tests: `tests/host` runs in `mise run test` (layouts, every format, the mapper
through `dlopen`, metadata across imports and `fork`, the frame dumper);
`tests/android` (`gralloc_test`, `hwc_test`) runs in `mise run test:bionic`
against the image's servicemanager and libui without booting. Start reading at
`common/src/layout.cpp`, `composer/Display.cpp` and
`composer/BridgePresenter.cpp`.
