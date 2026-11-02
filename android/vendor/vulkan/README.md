# vulkan

This directory builds the vendor Vulkan drivers from Mesa 26.2.5. RADV covers
AMD GPUs, ANV covers supported Gen9+ i915 Intel GPUs, and lavapipe covers
everything else. `mise run build:vulkan` cross-builds them with the NDK in the
build container (`build.sh`). It skips the build while the inputs hash to
`out/build/vulkan/.stamp`. The first LLVM build takes about 15 minutes. The
drivers install as `/vendor/lib64/hw/vulkan.{radv,anv,lvp}.so`, and libvulkan
loads one of them into every process that uses Vulkan. GLES is the image's ANGLE
on top of Vulkan (`ro.hardware.egl=angle`, in `etc/vulkan.prop`).

- **Self-contained drivers.** Each driver is one `.so`. libdrm, zlib and libc++
  are linked statically. Lavapipe also links LLVM statically. RADV uses ACO and
  ANV uses its own compiler, so they don't need LLVM. Every `DT_NEEDED` entry is
  an LL-NDK library, and the only export is `HMI`. `build.sh` checks both.
- **Native shader tools.** ANV's build needs `mesa_clc` and `vtn_bindgen2`.
  `build.sh` builds them from the same Mesa source against the container's LLVM
  and Clang, and doesn't ship them. Intel ray tracing is disabled.
- **Driver choice.** The launcher scans `/sys/class/drm/renderD*`. It picks the
  first amdgpu node for RADV, then an i915 node that ANV supports, and otherwise
  lavapipe. `share/innkeeper/anv-pci-ids` is generated from the pinned Mesa's
  device info, PCI table and ANV generation limits, so older and unknown Intel
  IDs fall back to software. `INNKEEPER_VULKAN=lvp` forces software rendering.
  The launcher saves the choice in session.conf. The broker lets Android open
  only that DRM device, so Mesa and the composer's ANGLE use the same GPU.
- **Features.** The launcher writes Vulkan permission XMLs into the runtime
  layer, alongside `ro.hardware.vulkan`. They declare Vulkan 1.3, compute level
  0, and hardware level 1 for RADV and ANV or level 0 for lavapipe.
- **`-Dandroid-strict=false`.** Strict mode caps the drivers at Vulkan 1.1 and
  the CTS extension allowlist. Without it, all three drivers report Vulkan 1.4.
- **Gralloc requirements.** The drivers need IMapper v5 metadata
  (`PIXEL_FORMAT_FOURCC`, `PLANE_LAYOUTS`, `DATASPACE`) and the 256-byte pitch
  ([docs/display.md](../../../docs/display.md#buffers)). The driver allocates
  GPU render targets with a tiled layout. It uses dedicated allocations where
  the driver requires them, as RADV does. CPU-accessed buffers stay on udmabuf.
  The mapper supplies the modifier and the explicit layout for LINEAR and tiled
  imports, so Mesa needs no Intel tiling or CCS override. Release fences are
  sync_files from DRM syncobjs, so sw_sync isn't needed.
- **Patches.** Each patch in `patches/mesa/` explains its reason in its header.
  - 0001 adds a stable-C IMapper v5 gralloc backend, so the drivers link neither
    libui nor libhardware.
  - 0002 makes lavapipe use the Vulkan ICD version script.
  - 0003 and 0004 support the deferred-allocation swapchain images that ANGLE
    uses. Without them, bootanimation crashed on RADV.
  - 0005 passes YUV hardware-buffer formats through the gralloc. Without it,
    video aborted SurfaceFlinger.
  - 0006 makes ANV's ANB imports use the mapper's explicit row pitches and
    offsets, without requiring the optional AHardwareBuffer pointer.
  - 0007 adds an `ohos` platform that leaves out display WSI (`VK_KHR_display`).
    That WSI calls `pthread_cancel`, which OpenHarmony's musl doesn't have, and
    musl resolves every symbol when it loads a library.
  - 0008 adds `VK_OHOS_native_buffer` to RADV and ANV, which OpenHarmony's
    swapchain layer requires ([OpenHarmony](#openharmony)).
  - 0009 adds an OpenHarmony EGL platform for Zink ([GL](#gl)).

Tests: `tests/run.sh` runs in `mise run test:bionic`. It runs `vkdriver_test`
(loading, compute, udmabuf import, ANB images, deferred binds),
`swapchain_test`, `ahb_vk_test`, `gles_test` and `gles_yuv_test`. The last
checks YV12, NV12 and NV21 through EGLImage, as Skia imports them. The tests run
on lavapipe and on the hardware driver that a normal launcher session selects.
On hybrid systems, that is the selected GPU alone, because the broker permits
only its node. Each invocation uses a fresh named Android session.
`tests/ohos.sh` runs in `mise run test:bionic` too. It runs
`ohos_swapchain_test` and `ohos_gles_test` in a fresh OpenHarmony profile on a
nested desktop, on the GPU that the launcher selects, and it is skipped without
a GPU. `ohos_gles_test` draws to an EGL window surface and imports a buffer with
`EGL_NATIVE_BUFFER_OHOS`, with our EGL loaded from a copy in `/data/local/tmp`.
It also imports NV12, NV21, I420 and YV12 buffers in a video frame's layout and
checks the RGB that a `samplerExternalOES` reads from them.

## OpenHarmony

The same build makes RADV and ANV for OpenHarmony
([ohos/README.md](../../../ohos/README.md#display)). They install as
`/vendor/lib64/chipsetsdk/libvulkan_{radeon,intel}.so`, with ICD manifests in
`/vendor/etc/vulkan/icd.d`. OpenHarmony's Vulkan loader reads manifests from
`/system/etc/vulkan/icd.d` and `/vendor/etc/vulkan/icd.d`, and each manifest
names its driver by absolute path.

OH's swapchain layer is always on, and it creates a device only on a driver with
`VK_OHOS_native_buffer`, OH's counterpart of Android's ANB extension. Patch 0008
adds it to RADV and ANV. The layer creates each swapchain image with a
`VkNativeBufferOHOS` that points to the buffer's `BufferHandle`. The handle has
a dma-buf fd and a row pitch, and no tiling, so the drivers import the buffer as
a linear image. The acquire and release fences are sync_files, as with ANB, and
the fence code of the two extensions is shared.

- **Toolchain.** The drivers build with the NDK's clang for `x86_64-linux-musl`
  (`cross/ohos-x86_64.ini`). The sysroot is the build container's musl with the
  container's kernel headers, and its `libc.so` gets the soname `libc.so` that
  OpenHarmony's libraries use. `build.sh` builds compiler-rt, libc++abi and
  libc++ for that sysroot from the pinned LLVM source, without exceptions.
- **Self-contained drivers.** libdrm and libc++ are linked statically. The only
  `DT_NEEDED` entry is `libc.so`, and the only exports are the three `vk_icd*`
  entry points. `build.sh` checks both. The drivers have no shader cache, zlib
  or window-system integration.

### GL

The same Mesa build makes EGL, GLES and Zink for OpenHarmony. OH's EGL wrapper
(`/system/lib64/platformsdk/libEGL.so`) loads `libEGL_impl.so` and
`libGLESv{1,2,3}_impl.so` from `/vendor/lib64/chipsetsdk`. `build.sh` installs
ours under those names, with `libgallium-26.2.5.so`, in
`share/innkeeper/ohos/gpu/vendor`, a tree apart from the Vulkan drivers.

- **Platform.** Patch 0009 adds an `ohos` EGL platform, and `build.sh` makes it
  the native platform. Its driver is Zink. A window surface is a kopper drawable
  with `VK_OHOS_surface`, which OH's swapchain layer provides, so its buffers
  reach the Vulkan driver through `VK_OHOS_native_buffer`. The layer has no BGRA
  formats, so the window configs are RGBA. The patch ports the surface and
  kopper code of OpenHarmony's Mesa
  ([third_party_mesa3d](https://gitee.com/openharmony/third_party_mesa3d), MIT).
- **Native buffers.** render_service imports other surfaces' buffers with
  `eglCreateImageKHR(EGL_NATIVE_BUFFER_OHOS)`. The platform imports the buffer's
  dma-buf as a linear image of its RGB format with the handle's row pitch. Video
  frames are YUV 4:2:0 (NV12, NV21, I420 or YV12), with the chroma after the
  luma in the same dma-buf, so the platform imports them as two or three planes
  of it. They have no color metadata and import as BT.601 limited range, as
  Android's gralloc fallback does. render_service samples them as external
  textures.
- **NDK.** The platform calls OH's NativeWindow API in `libnative_window.so`.
  `build.sh` copies OH's NDK headers for it (`android/vendor/third_party/ohos`)
  into the sysroot, with a link stub that has the library's soname. Each library
  needs only `libc.so`, `libgallium-26.2.5.so` and, for EGL,
  `libnative_window.so`, and `build.sh` checks that.
