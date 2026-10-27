# GPU drivers (gpu lane)

Vendor Vulkan HAL drivers built out of tree with the NDK. GLES comes from the
ANGLE that ships in `/system`, running on top of whichever Vulkan driver is
installed.

| Driver                      | File                                      | For                                  |
| --------------------------- | ----------------------------------------- | ------------------------------------ |
| Mesa RADV (ACO, no LLVM)    | `/vendor/lib64/hw/vulkan.radv.so` (19 MB) | AMD GCN/RDNA via `/dev/dri/renderD*` |
| Mesa lavapipe (LLVM 19 JIT) | `/vendor/lib64/hw/vulkan.lvp.so` (47 MB)  | any machine, headless CI             |

Both drivers report Vulkan 1.4. Each one is a single file: libdrm and
libdrm_amdgpu, zlib, LLVM and libc++ are linked statically. `DT_NEEDED` lists
only LL-NDK libraries (`liblog libnativewindow libsync libm libdl libc`), and
the only exported symbol is `HMI`. `build.sh` checks both.

## Build

```
gpu/build.sh          # libdrm + RADV, LLVM + lavapipe, etc files, tests
gpu/build.sh radv     # just RADV (about 2 min); also: lvp, llvm, test
```

- Runs in a rootless podman image (`gpu/Containerfile`, via `gpu/tools.sh`). It
  uses the host NDK at `~/Android/Sdk/ndk/<pin>`.
- Sources and build trees go in `out/gpu/{src,build,stage}`, and the output goes
  in `out/vendor-overlay/`.
- Pins in `build.sh`: Mesa `mesa-26.2.5` (ddaf85e), libdrm `libdrm-2.4.134`,
  LLVM `llvmorg-19.1.7` (the same LLVM as Mesa CI's Android build), NDK r30
  `30.0.16248370`, API 37.
- The first LLVM build takes about 15 minutes. Run long builds as
  `systemd-run --user --unit=lapk-gpu-build --collect ...`.
- Mesa options:
  `-Dplatforms=android -Dandroid-stub=true -Dandroid-strict=false -Dplatform-sdk-version=37`.
  RADV uses `-Dllvm=disabled`, and lavapipe uses a static LLVM through a meson
  subproject wrap.
  - `android-strict=false` is deliberate. Strict mode caps the drivers at Vulkan
    1.1 and an Android CTS extension allowlist.

### Mesa patches (`gpu/patches/mesa/`)

1. **u_gralloc: stable-C IMapper v5 backend; libhardware at runtime.**
   - Mesa's own IMapper v5 path uses libui, which isn't LL-NDK, so it's unusable
     out of tree. The new backend `u_gralloc_imapper5_stablec.c` loads
     `mapper.<suffix>.so` through `android_load_sphal_library`, calls
     `AIMapper_loadIMapper`, and decodes the StandardMetadata encoding itself.
   - The suffix comes from the IAllocator AIDL service's
     `getIMapperLibrarySuffix()` (a raw libbinder_ndk transaction, the way libui
     does it). The option `MESA_GRALLOC_MAPPER_SUFFIX` (property
     `mesa.gralloc.mapper.suffix`) overrides it.
   - `hw_get_module` is now looked up with dlopen when needed, so the driver no
     longer links libhardware.
2. **lavapipe: link with the Vulkan ICD version script**, so it exports only
   `HMI` and not the 22k LLVM and libc++ symbols.
3. **radv/android: deferred-allocation ANB swapchain images.** This fixes the
   bootanimation crash (SIGSEGV at 0x60 in `vkAcquireNextImageKHR`), see below.
4. **lavapipe: use the gralloc layout for deferred ANB images.** Before this,
   deferred images got lavapipe's own row pitch (448 B instead of the gralloc's
   512 B for 100 px).

## Swapchains, the bootanimation crash, and fences

**The crash.** ANGLE creates its swapchain with
`VK_SWAPCHAIN_CREATE_DEFERRED_MEMORY_ALLOCATION_BIT_EXT`.

- libvulkan then creates each image with only `VkImageSwapchainCreateInfoKHR`.
- When `vkAcquireNextImageKHR` first dequeues a gralloc buffer, libvulkan binds
  it with `vkBindImageMemory2(memory = VK_NULL_HANDLE)`, chaining
  `VkNativeBufferANDROID` → `VkBindImageMemorySwapchainInfoKHR`.
- RADV had no Android handling for this. It laid the image out as an ordinary
  optimal image, then read `mem->alloc_size` through the NULL memory (offset
  0x60) in `radv_BindImageMemory2`.
- Patch 0003 makes RADV defer the layout (`vk_android_init_deferred_image`). At
  bind time it lays the image out from the gralloc metadata
  (`vk_android_get_anb_layout`, i.e. our IMapper v5) and binds the imported
  dma-buf, the same way turnip does.
- `vkdriver_test --anb --deferred` reproduces the crash in five calls: the same
  fault at 0x60 before the patch, PASS after.

**Fences: RADV needs no sw_sync.**

- `vkQueueSignalReleaseImageANDROID` (Mesa's common implementation) exports the
  release fence as a sync_file from an amdgpu DRM syncobj, a real kernel
  dma_fence. Every presented frame in `swapchain_test` carried a real sync_file
  to the BufferQueue consumer (120/120). That was true for lavapipe as well.
- `vkAcquireImageANDROID` imports the dequeue fence as a temporary `SYNC_FD`
  payload; `-1` means "already signalled", which is valid.
- So producers hand SurfaceFlinger and the composer real sync_files, and the
  composer can wait on them with `poll()`.
- Only a component that must create fences from nothing needs sw_sync, e.g. the
  composer's present/release fences. It can instead use the GPU
  (`vkGetSemaphoreFdKHR(SYNC_FD)` on a submitted semaphore) or
  `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` on the buffer.
- RADV attaches implicit fences to dma-bufs it writes, because imported BOs
  without `AMDGPU_GEM_CREATE_EXPLICIT_SYNC` get `RADEON_FLAG_IMPLICIT_SYNC`. So
  `DMA_BUF_IOCTL_EXPORT_SYNC_FILE(DMA_BUF_SYNC_READ)` on a gralloc buffer gives
  a fence for the GPU's pending writes. Not yet exercised by a test.

## Properties (`out/vendor-overlay/etc/gpu.prop`)

| Property              | Value                                       | Effect                                         |
| --------------------- | ------------------------------------------- | ---------------------------------------------- |
| `ro.hardware.vulkan`  | `radv`, or `lvp` with no amdgpu render node | libvulkan loads `vulkan.<value>.so` from sphal |
| `ro.hardware.egl`     | `angle`                                     | libEGL loads the system ANGLE                  |
| `ro.opengles.version` | `196610`                                    | GLES 3.2                                       |

The session lane should choose `radv` or `lvp` at session start: use `radv` when
`/sys/dev/char/<render node>/device/driver` is `amdgpu`, else `lvp`.

The overlay also installs
`etc/permissions/android.hardware.vulkan.{version (1.3), level (1), compute (0)}.xml`.
With `lvp`, level 0 would be the honest value.

## Runtime evidence (2026-10-27, gfx1151)

`gpu/test/run-bionic.sh` runs Bionic binaries in a rootless user+mount+pid
namespace, without booting Android:

- It provides the image's `/system`, the flattened `/apex` (`out/root/apex`),
  our `/vendor` and only the render nodes.
- `/sys` is a read-only tmpfs with read-only binds of just the GPU's PCI device
  dir and the CPU topology (see the host-safety rules in NOTES.md).
- The image's own `linkerconfig` generates `/linkerconfig`, and tests live in
  `/data/gputest`, so they run in the `[system]` section with a real `sphal`
  namespace.

`gpu/test/vkdriver_test` covers the following (all PASS):

| Test                                                                                                                                                           | RADV                                                          | lavapipe                                    |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- | ------------------------------------------- |
| HAL load like libvulkan (`android_load_sphal_library("vulkan.X.so")`, `HMI`, open `vk0`), instance and device                                                  | `AMD Radeon 8060S Graphics (RADV STRIX_HALO)`, Vulkan 1.4.354 | `llvmpipe (LLVM 19.1.7, 256 bits)`, 1.4.354 |
| Compute dispatch of 1M invocations, host readback                                                                                                              | 0 mismatches                                                  | 0 mismatches                                |
| Import a memfd→udmabuf dma-buf as VkDeviceMemory, compute writes, check through the memfd mmap                                                                 | memoryTypeBits 0x420 (GTT), 0 mismatches                      | 0 mismatches                                |
| `VkNativeBufferANDROID` image (the swapchain path) from a gralloc handle, GPU copy, check pixels and row padding through the mmap                              | PASS                                                          | PASS                                        |
| Same, but with the hal lane's allocator code and the real `mapper.linuxapk.so` (`--anb-hal`)                                                                   | PASS                                                          | PASS                                        |
| Through the system `libvulkan.so` (`--loader`, properties via `test/propshim.c`)                                                                               | PASS                                                          |                                             |
| libvulkan's deferred-allocation bind (`--anb --deferred`, `--anb-hal --deferred`)                                                                              | PASS (crashed at 0x60 before patch 0003)                      | PASS (wrong pitch before patch 0004)        |
| `swapchain_test`: 120-frame acquire, clear, present loop on an `AImageReader` window, consumer checks every frame (`run-bionic.sh -s`, eager and `--deferred`) | PASS, 120/120 real release fences                             | PASS                                        |

The ANB test passes the driver a deliberately wrong pixel stride, so it can only
pass if the driver got `PLANE_LAYOUTS` from the IMapper. Without a mapper,
Mesa's fallback gralloc fails with
`VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`.

### Inside the session (2026-10-27)

The session was partially booted: servicemanager, `IAllocator/default` and `gpu`
were up; there were no props yet, so `propshim` supplied them. The tests ran
from `/data/gputest`.

- **GLES through ANGLE on RADV works.** `gles_test` reports
  `GL_RENDERER=ANGLE (AMD, Vulkan 1.4.354 (AMD Radeon 8060S Graphics (RADV STRIX_HALO) (0x00001586)), radv-26.2.5)`
  and `OpenGL ES 3.2`, and the pbuffer clear and scissor readback passes.
  (Outside a session it blocks: libui waits for IAllocator.)
- Compute through the system libvulkan also passes.
- Mesa found the mapper through the IAllocator binder query, with no override.
  Image layout came from `mapper.linuxapk.so`.
- **Blocker: the session has no `/dev/udmabuf`.** The allocator fell back to
  plain memfd, and a memfd can't be imported as a dma-buf
  (`VK_ERROR_INVALID_EXTERNAL_HANDLE`), so swapchain buffers would fail.
  - Session lane: bind `/dev/udmabuf` into the session.
  - hal lane: fail allocations with GPU or composer usage rather than hand out
    memfd buffers.

Later the same day, in a session with SurfaceFlinger and `/dev/udmabuf`, running
the patched driver:

- `swapchain_test` passes on RADV and lavapipe, eager and `--deferred`: 120
  frames each, 0 bad pixels, and a real release sync_file on every frame.
- `gles_test` (ANGLE on RADV) passes.
- **bootanimation, the original crash, now runs.** I started it by hand with
  `debug.sf.nobootanimation=0` from the shim. It ran until killed after 12 s,
  with no tombstone. The composer's frame dump (`vendor.linuxapk.hwc.dump.dir`)
  shows the "android" logo with its shimmer at 1280x800. It was drawn by ANGLE
  on RADV and composited by SurfaceFlinger (copy in
  `out/gpu/bootanimation-frame.png`).

```
gpu/build.sh test
TIMEOUT=120 gpu/test/run-bionic.sh -e MESA_GRALLOC_MAPPER_SUFFIX=linuxapk -- \
  /data/gputest/vkdriver_test --hal vulkan.radv.so --udmabuf --anb-hal --deferred
# Swapchain loop without a session: -s starts servicemanager and our allocator.
# test/fake_sf.c stands in for SurfaceFlinger's getDisplayStats.
TIMEOUT=90 gpu/test/run-bionic.sh -s -e LINUXAPK_PROPS=ro.hardware.vulkan=radv -- \
  /system/bin/sh -c '/data/gputest/fake_sf & sleep 0.5; /data/gputest/swapchain_test --deferred'
```

## Gralloc guidance for the hal lane

**udmabuf works with RADV on amdgpu.**

- Kernel 7.1 imports udmabuf dma-bufs as foreign SG BOs.
  `vkGetMemoryFdPropertiesKHR` returns the GTT memory types (0x420).
- The GPU wrote into them, and the CPU saw the data through the memfd mapping
  with no explicit cache maintenance, on this APU.
- Keep memfd + udmabuf as the default allocator. Lavapipe imports the same
  buffers by mmap.

What the drivers need from the gralloc:

1. **IMapper v5 `getStandardMetadata`** must answer `PIXEL_FORMAT_FOURCC`,
   `PIXEL_FORMAT_MODIFIER` (`DRM_FORMAT_MOD_LINEAR`) and `PLANE_LAYOUTS`
   (`offsetInBytes`, `strideInBytes`); `DATASPACE` and `CHROMA_SITING` are
   optional.
   - It's called on handles the process has imported. Mesa loads the mapper in
     the same sphal namespace as libui, so they share one instance and one
     registry. `mapper.linuxapk.so` already does all of this.
   - Planes at offset 0 after plane 0 are taken to be in the next fd. Disjoint
     multi-plane buffers are rejected, so keep YUV planes in one fd.
2. **Row pitch must be a multiple of 256 bytes for RADV.**
   - Measured with `--anb-stride` on 100-px-wide RGBA8888 images: 400, 416, 448
     and 640 B fail with
     `VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`; 512, 768, 1024 and
     1280 B work. Lavapipe accepts any pitch.
   - The current `kRowAlign = 256` is right.
   - YV12's chroma rule (luma stride / 2, aligned to 16) gives non-256-aligned
     chroma pitches. Expect RADV to reject YV12 images unless the chroma pitch
     is 256-aligned too. Not yet tested.
3. **IAllocator AIDL** must implement `getIMapperLibrarySuffix()` (V2+). Mesa
   finds the mapper with it, so the driver needs no configuration.
4. Accept the gralloc usages libvulkan asks for on swapchain images:
   `GPU_RENDER_TARGET | GPU_TEXTURE`, plus `FRONT_BUFFER` (1<<32) for shared
   presentation.

When to allocate from amdgpu GEM instead (not needed on this machine):

- On a discrete GPU, system-memory buffers mean every render-target access
  crosses PCIe.
- For buffers with `GPU_RENDER_TARGET` or `COMPOSER_*` usage on a dGPU, allocate
  with `DRM_IOCTL_AMDGPU_GEM_CREATE` on the render node:
  - domain VRAM|GTT, `AMDGPU_GEM_CREATE_CPU_ACCESS_REQUIRED` for CPU lock;
  - linear, same 256-byte pitch;
  - exported with `DRM_IOCTL_PRIME_HANDLE_TO_FD` (`DRM_CLOEXEC|DRM_RDWR`), and
    mmap the dma-buf for CPU access.
- Keep the same handle layout, so nothing changes for Mesa, the composer or
  Wayland.
