# vulkan

The vendor Vulkan drivers: Mesa (26.2.5) RADV for AMD GPUs, ANV for supported
Gen9+ i915 Intel GPUs and lavapipe for everything else, cross-built with the NDK
by `mise run build:vulkan` (`build.sh`, in the build container; skipped while
its inputs hash to `out/build/vulkan/.stamp`; the first LLVM build takes about
15 minutes). They install as `/vendor/lib64/hw/vulkan.{radv,anv,lvp}.so` and
libvulkan loads one into every process that uses Vulkan. GLES comes from the
image's ANGLE on top (`ro.hardware.egl=angle`, in `etc/vulkan.prop`).

- **One self-contained `.so` per driver.** libdrm, zlib, LLVM (lavapipe only;
  RADV uses ACO; ANV uses its own compiler) and libc++ are linked statically;
  `DT_NEEDED` is LL-NDK only and the only export is `HMI`. `build.sh` checks
  both.
- **Native shader tools:** `mesa_clc` and `vtn_bindgen2` are built from the same
  Mesa source with the container’s native LLVM/Clang, then used by the NDK ANV
  build. They are never shipped. Intel ray tracing is disabled.
- **Driver choice:** the launcher scans every `/sys/class/drm/renderD*` and
  selects the first amdgpu node (RADV), then an i915 node supported by ANV, else
  lavapipe. The build generates `share/linuxapk/anv-pci-ids` from the pinned
  Mesa device info, PCI table and ANV generation limits; older and unknown Intel
  IDs keep the software fallback. `LINUXAPK_VULKAN=lvp` forces software
  rendering. The choice is saved in session.conf: the broker permits Android to
  open only that DRM device, so Mesa and the composer's ANGLE use the same GPU.
  Gralloc uses driver-allocated tiled RGB render targets where the driver can
  export them without a dedicated allocation (ANV, not RADV), keeping udmabuf
  for CPU-accessed buffers. Flatpak's existing DRM grant to the desktop bridge
  stays the same.
- **Features:** the launcher writes Vulkan permission XMLs into the runtime
  layer alongside `ro.hardware.vulkan`: Vulkan 1.3, compute level 0, and
  hardware level 1 for RADV/ANV or 0 for lavapipe.
- **`-Dandroid-strict=false`** on purpose: strict mode caps the drivers at
  Vulkan 1.1 and the CTS extension allowlist. All three report Vulkan 1.4.
- **What the drivers need from the gralloc:** IMapper v5 metadata
  (`PIXEL_FORMAT_FOURCC`, `PLANE_LAYOUTS`, `DATASPACE`) and the 256-byte pitch
  ([docs/display.md](../../../docs/display.md#buffers)). Release fences are real
  sync_files from DRM syncobjs, so no sw_sync is needed.
- **Patches** (`patches/mesa/`, each with its rationale in the header): a
  stable-C IMapper v5 gralloc backend so the drivers link no libui or
  libhardware (0001); lavapipe's ICD version script (0002); deferred-allocation
  swapchain images that ANGLE uses, which crashed bootanimation on RADV (0003,
  0004); YUV hardware-buffer formats through the gralloc, without which video
  aborted SurfaceFlinger (0005); ANV’s regular and deferred ANB imports use the
  mapper’s explicit row pitches and offsets, without requiring the optional
  AHardwareBuffer pointer (0006). The mapper supplies the allocated modifier and
  explicit layout for both LINEAR and tiled imports; no Intel tiling or CCS
  override is needed in Mesa.

Tests: `tests/run.sh` (in `mise run test:bionic`) runs `vkdriver_test` (loading,
compute, udmabuf import, ANB images, deferred binds), `swapchain_test`,
`gles_test` and `gles_yuv_test` (YV12, NV12, NV21 through EGLImage as Skia
imports them) on lavapipe plus the hardware driver selected by a normal launcher
session. On hybrids this tests only the selected GPU, since the broker permits
only its node. Each invocation uses a fresh named Android session; the optional
`LINUXAPK_BIONIC_HARNESS=1` runner runs without booting Android.
