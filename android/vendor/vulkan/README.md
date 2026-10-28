# vulkan

The vendor Vulkan drivers: Mesa (26.2.5) RADV for AMD GPUs and lavapipe for
everything else, cross-built with the NDK by `mise run build:vulkan`
(`build.sh`, in the build container; skipped while its inputs hash to
`out/build/vulkan/.stamp`; the first LLVM build takes about 15 minutes). They
install as `/vendor/lib64/hw/vulkan.{radv,lvp}.so` and libvulkan loads one into
every process that uses Vulkan. GLES comes from the image's ANGLE on top
(`ro.hardware.egl=angle`, in `etc/vulkan.prop`).

- **One self-contained `.so` per driver.** libdrm, zlib, LLVM (lavapipe only;
  RADV uses ACO) and libc++ are linked statically; `DT_NEEDED` is LL-NDK only
  and the only export is `HMI`. `build.sh` checks both.
- **Driver choice:** the launcher sets `ro.hardware.vulkan=radv` when the render
  node's kernel driver is amdgpu, else `lvp`.
- **`-Dandroid-strict=false`** on purpose: strict mode caps the drivers at
  Vulkan 1.1 and the CTS extension allowlist. Both report Vulkan 1.4.
- **What the drivers need from the gralloc:** IMapper v5 metadata
  (`PIXEL_FORMAT_FOURCC`, `PLANE_LAYOUTS`, `DATASPACE`) and the 256-byte pitch
  ([docs/display.md](../../../docs/display.md#buffers)). Release fences are real
  sync_files from amdgpu syncobjs, so no sw_sync is needed.
- **Patches** (`patches/mesa/`, each with its rationale in the header): a
  stable-C IMapper v5 gralloc backend so the drivers link no libui or
  libhardware (0001); lavapipe's ICD version script (0002); deferred-allocation
  swapchain images that ANGLE uses, which crashed bootanimation on RADV (0003,
  0004); YUV hardware-buffer formats through the gralloc, without which video
  aborted SurfaceFlinger (0005).

Tests: `tests/run.sh` (in `mise run test:bionic`) runs `vkdriver_test` (loading,
compute, udmabuf import, ANB images, deferred binds), `swapchain_test`,
`gles_test` and `gles_yuv_test` (YV12, NV12, NV21 through EGLImage as Skia
imports them) on lavapipe, and on RADV when there is an amdgpu render node,
without booting Android.
