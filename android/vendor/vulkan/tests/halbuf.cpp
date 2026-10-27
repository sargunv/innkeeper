// Test helper for vkdriver_test --anb-hal: allocate a buffer with the hal
// lane's allocator code (memfd -> udmabuf, linuxapk handle layout) and import
// it through the real mapper.linuxapk.so, as libui would in an app process.
// Built from the graphics HAL sources by android/vendor/vulkan/build.sh.
#include <dlfcn.h>
#include <stdio.h>
#include <sys/mman.h>

#include "linuxapk/buffer_alloc.h"
#include "linuxapk/gralloc_handle.h"

namespace {
struct AIMapperV5Subset {
  int32_t (*importBuffer)(const native_handle_t*, const native_handle_t**);
  void* rest[14];
};
struct AIMapperSubset {
  alignas(alignof(max_align_t)) uint32_t version;
  AIMapperV5Subset v5;
};
}  // namespace

extern "C" __attribute__((visibility("default"))) int halbuf_alloc(
  int width, int height, const native_handle_t** out_imported,
  int* out_stride_px, size_t* out_stride_bytes, void** out_map,
  size_t* out_map_size
) {
  using namespace linuxapk;
  AllocRequest req{
    "vkdriver_test",
    uint32_t(width),
    uint32_t(height),
    1,
    1 /* RGBA_8888 */,
    0x300 /* GPU_TEXTURE | GPU_RENDER_TARGET */ | 0x33
    /* CPU read/write often */,
    0
  };
  native_handle_t* raw = nullptr;
  if (allocateBuffer(req, &raw) != AllocStatus::Ok) {
    printf("halbuf: allocateBuffer failed\n");
    return -1;
  }
  const BufferHandle* bh = asBufferHandle(raw);
  printf(
    "halbuf: allocated %ux%u fourcc 0x%08x stride %u px / %u B, dmabuf=%d "
    "(udmabuf %s)\n",
    bh->width, bh->height, bh->fourcc, bh->stridePixels, bh->planes[0].stride,
    bh->fd, (bh->flags & kFlagDmabuf) ? "yes" : "NO, memfd"
  );

  // Same loader path as libui and Mesa, so all three share one mapper instance.
  void* vndk = dlopen("libvndksupport.so", RTLD_NOW);
  auto load_sphal = vndk ? reinterpret_cast<void* (*)(const char*, int)>(
                             dlsym(vndk, "android_load_sphal_library")
                           )
                         : nullptr;
  void* so = load_sphal
               ? load_sphal("mapper.linuxapk.so", RTLD_NOW | RTLD_LOCAL)
               : nullptr;
  auto load = so ? reinterpret_cast<int32_t (*)(AIMapperSubset**)>(
                     dlsym(so, "AIMapper_loadIMapper")
                   )
                 : nullptr;
  AIMapperSubset* mapper = nullptr;
  if (!load || load(&mapper) != 0 || mapper->version < 5) {
    printf("halbuf: cannot load mapper.linuxapk.so: %s\n", dlerror());
    return -1;
  }
  if (mapper->v5.importBuffer(raw, out_imported) != 0) {
    printf("halbuf: importBuffer failed\n");
    return -1;
  }
  *out_stride_px = int(bh->stridePixels);
  *out_stride_bytes = bh->planes[0].stride;
  *out_map_size = bh->planes[0].stride * size_t(bh->height);
  *out_map =
    mmap(nullptr, *out_map_size, PROT_READ | PROT_WRITE, MAP_SHARED, bh->fd, 0);
  return *out_map == MAP_FAILED ? -1 : 0;
}
