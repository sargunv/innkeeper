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

Tests: `tests/run.sh` runs in `mise run test:bionic`. It runs `vkdriver_test`
(loading, compute, udmabuf import, ANB images, deferred binds),
`swapchain_test`, `ahb_vk_test`, `gles_test` and `gles_yuv_test`. The last
checks YV12, NV12 and NV21 through EGLImage, as Skia imports them. The tests run
on lavapipe and on the hardware driver that a normal launcher session selects.
On hybrid systems, that is the selected GPU alone, because the broker permits
only its node. Each invocation uses a fresh named Android session.

## OpenHarmony

The same build makes RADV and ANV for OpenHarmony
([ohos/README.md](../../../ohos/README.md#display)). They install as
`/vendor/lib64/chipsetsdk/libvulkan_{radeon,intel}.so`, with ICD manifests in
`/vendor/etc/vulkan/icd.d`. OpenHarmony's Vulkan loader reads manifests from
`/system/etc/vulkan/icd.d` and `/vendor/etc/vulkan/icd.d`, and each manifest
names its driver by absolute path. The loader lists the GPU, but OH's swapchain
layer refuses to create a device until the drivers support
`VK_OHOS_native_buffer`.

- **Toolchain.** The drivers build with the NDK's clang for `x86_64-linux-musl`
  (`cross/ohos-x86_64.ini`). The sysroot is the build container's musl with the
  container's kernel headers, and its `libc.so` gets the soname `libc.so` that
  OpenHarmony's libraries use. `build.sh` builds compiler-rt, libc++abi and
  libc++ for that sysroot from the pinned LLVM source, without exceptions.
- **Self-contained drivers.** libdrm and libc++ are linked statically. The only
  `DT_NEEDED` entry is `libc.so`, and the only exports are the three `vk_icd*`
  entry points. `build.sh` checks both. The drivers have no shader cache, zlib
  or window-system integration.
