# GPU drivers (gpu lane)

Vendor Vulkan HAL drivers built out of tree with the NDK. GLES comes from the
ANGLE that ships in `/system`, running on top of whichever Vulkan driver is
installed.

| Driver | File | For |
|---|---|---|
| Mesa RADV (ACO, no LLVM) | `/vendor/lib64/hw/vulkan.radv.so` (19 MB) | AMD GCN/RDNA via `/dev/dri/renderD*` |
| Mesa lavapipe (LLVM 19 JIT) | `/vendor/lib64/hw/vulkan.lvp.so` (47 MB) | any machine, headless CI |

Both drivers report Vulkan 1.4. Each one is a single file: libdrm and
libdrm_amdgpu, zlib, LLVM and libc++ are linked statically. `DT_NEEDED` lists
only LL-NDK libraries (`liblog libnativewindow libsync libm libdl libc`), and
the only exported symbol is `HMI`. `build.sh` checks both.

## Build

```
gpu/build.sh          # libdrm + RADV, LLVM + lavapipe, etc files, tests
gpu/build.sh radv     # just RADV (about 2 min); also: lvp, llvm, test
```

- Runs in a rootless podman image (`gpu/Containerfile`, via `gpu/tools.sh`).
  It uses the host NDK at `~/Android/Sdk/ndk/<pin>`.
- Sources and build trees go in `out/gpu/{src,build,stage}`, and the output
  goes in `out/vendor-overlay/`.
- Pins in `build.sh`: Mesa `mesa-26.2.5` (ddaf85e), libdrm `libdrm-2.4.134`,
  LLVM `llvmorg-19.1.7` (the same LLVM as Mesa CI's Android build), NDK r30
  `30.0.16248370`, API 37.
- The first LLVM build takes about 15 minutes. Run long builds as
  `systemd-run --user --unit=lapk-gpu-build --collect ...`.
- Mesa options: `-Dplatforms=android -Dandroid-stub=true -Dandroid-strict=false
  -Dplatform-sdk-version=37`. RADV uses `-Dllvm=disabled`, and lavapipe uses a
  static LLVM through a meson subproject wrap.
  - `android-strict=false` is deliberate. Strict mode caps the drivers at
    Vulkan 1.1 and an Android CTS extension allowlist.

### Mesa patches (`gpu/patches/mesa/`)

1. **u_gralloc: stable-C IMapper v5 backend; libhardware at runtime.**
   - Mesa's own IMapper v5 path uses libui, which isn't LL-NDK, so it's
     unusable out of tree. The new backend `u_gralloc_imapper5_stablec.c`
     loads `mapper.<suffix>.so` through `android_load_sphal_library`, calls
     `AIMapper_loadIMapper`, and decodes the StandardMetadata encoding itself.
   - The suffix comes from the IAllocator AIDL service's
     `getIMapperLibrarySuffix()` (a raw libbinder_ndk transaction, the way
     libui does it). The option `MESA_GRALLOC_MAPPER_SUFFIX` (property
     `mesa.gralloc.mapper.suffix`) overrides it.
   - `hw_get_module` is now looked up with dlopen when needed, so the driver no
     longer links libhardware.
2. **lavapipe: link with the Vulkan ICD version script**, so it exports only
   `HMI` and not the 22k LLVM and libc++ symbols.

## Properties (`out/vendor-overlay/etc/gpu.prop`)

| Property | Value | Effect |
|---|---|---|
| `ro.hardware.vulkan` | `radv`, or `lvp` with no amdgpu render node | libvulkan loads `vulkan.<value>.so` from sphal |
| `ro.hardware.egl` | `angle` | libEGL loads the system ANGLE |
| `ro.opengles.version` | `196610` | GLES 3.2 |

The session lane should choose `radv` or `lvp` at session start: use `radv`
when `/sys/dev/char/<render node>/device/driver` is `amdgpu`, else `lvp`.

The overlay also installs `etc/permissions/android.hardware.vulkan.{version
(1.3), level (1), compute (0)}.xml`. With `lvp`, level 0 would be the honest
value.

## Runtime evidence (2026-10-27, gfx1151)

`gpu/test/run-bionic.sh` runs Bionic binaries in a rootless
user+mount+pid namespace, without booting Android:

- It provides the image's `/system`, the flattened `/apex` (`out/root/apex`),
  our `/vendor` and only the render nodes.
- `/sys` is a read-only tmpfs with read-only binds of just the GPU's PCI
  device dir and the CPU topology (see the host-safety rules in NOTES.md).
- The image's own `linkerconfig` generates `/linkerconfig`, and tests live in
  `/data/gputest`, so they run in the `[system]` section with a real
  `sphal` namespace.

`gpu/test/vkdriver_test` covers the following (all PASS):

| Test | RADV | lavapipe |
|---|---|---|
| HAL load like libvulkan (`android_load_sphal_library("vulkan.X.so")`, `HMI`, open `vk0`), instance and device | `AMD Radeon 8060S Graphics (RADV STRIX_HALO)`, Vulkan 1.4.354 | `llvmpipe (LLVM 19.1.7, 256 bits)`, 1.4.354 |
| Compute dispatch of 1M invocations, host readback | 0 mismatches | 0 mismatches |
| Import a memfd→udmabuf dma-buf as VkDeviceMemory, compute writes, check through the memfd mmap | memoryTypeBits 0x420 (GTT), 0 mismatches | 0 mismatches |
| `VkNativeBufferANDROID` image (the swapchain path) from a gralloc handle, GPU copy, check pixels and row padding through the mmap | PASS | PASS |
| Same, but with the hal lane's allocator code and the real `mapper.linuxapk.so` (`--anb-hal`) | PASS | PASS |
| Through the system `libvulkan.so` (`--loader`, properties via `test/propshim.c`) | PASS | |

The ANB test passes the driver a deliberately wrong pixel stride, so it can
only pass if the driver got `PLANE_LAYOUTS` from the IMapper. Without a
mapper, Mesa's fallback gralloc fails with
`VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`.

Not yet tested: GLES through ANGLE. `gles_test` reaches ANGLE's Vulkan init on
RADV, then blocks in `AHardwareBuffer_isSupported` → libui → waiting for
IAllocator via servicemanager. It needs a booted session with the allocator
running.

```
gpu/build.sh test
TIMEOUT=120 gpu/test/run-bionic.sh -e MESA_GRALLOC_MAPPER_SUFFIX=linuxapk -- \
  /data/gputest/vkdriver_test --hal vulkan.radv.so --udmabuf --anb-hal
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
   - Planes at offset 0 after plane 0 are taken to be in the next fd.
     Disjoint multi-plane buffers are rejected, so keep YUV planes in one fd.
2. **Row pitch must be a multiple of 256 bytes for RADV.**
   - Measured with `--anb-stride` on 100-px-wide RGBA8888 images: 400, 416,
     448 and 640 B fail with `VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`;
     512, 768, 1024 and 1280 B work. Lavapipe accepts any pitch.
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
- For buffers with `GPU_RENDER_TARGET` or `COMPOSER_*` usage on a dGPU,
  allocate with `DRM_IOCTL_AMDGPU_GEM_CREATE` on the render node:
  - domain VRAM|GTT, `AMDGPU_GEM_CREATE_CPU_ACCESS_REQUIRED` for CPU lock;
  - linear, same 256-byte pitch;
  - exported with `DRM_IOCTL_PRIME_HANDLE_TO_FD` (`DRM_CLOEXEC|DRM_RDWR`), and
    mmap the dma-buf for CPU access.
- Keep the same handle layout, so nothing changes for Mesa, the composer or
  Wayland.
