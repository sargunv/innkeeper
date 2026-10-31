// SPDX-License-Identifier: AGPL-3.0-or-later
// On-device gralloc test. Needs servicemanager and our allocator running.
//  1. IAllocator over binder + mapper.innkeeper.so via dlopen (what libui
//  does).
//  2. The public AHardwareBuffer API, i.e. libnativewindow -> libui Gralloc5 ->
//     our allocator, with the mapper found through the VINTF manifest.
#include <string>
#include <vector>

#include <aidl/android/hardware/graphics/allocator/AllocationError.h>
#include <aidl/android/hardware/graphics/allocator/IAllocator.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <android/hardware/graphics/mapper/IMapper.h>
#include <android/hardware/graphics/mapper/utils/IMapperMetadataTypes.h>
#include <android/hardware_buffer.h>
#include <dlfcn.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "innkeeper/gralloc_handle.h"
#include "innkeeper/native_handle.h"

using namespace aidl::android::hardware::graphics::allocator;
using namespace android::hardware::graphics::mapper;
using aidl::android::hardware::common::NativeHandle;

static int gFail = 0, gChecks = 0;
#define CHECK(c)                                                   \
  do {                                                             \
    gChecks++;                                                     \
    if (!(c)) {                                                    \
      gFail++;                                                     \
      fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
    }                                                              \
  } while (0)

static AIMapper* gMapper;

template <StandardMetadataType T>
static auto get(buffer_handle_t h)
  -> decltype(StandardMetadata<T>::value::decode(nullptr, 0)) {
  std::vector<uint8_t> buf(128);
  int32_t n =
    gMapper->v5.getStandardMetadata(h, int64_t(T), buf.data(), buf.size());
  if (n > 0 && size_t(n) > buf.size()) {
    buf.resize(n);
    n = gMapper->v5.getStandardMetadata(h, int64_t(T), buf.data(), buf.size());
  }
  if (n < 0 || size_t(n) > buf.size()) return std::nullopt;
  return StandardMetadata<T>::value::decode(buf.data(), n);
}

static bool isUdmabuf(int fd) {
  char path[64], line[256];
  snprintf(path, sizeof path, "/proc/self/fdinfo/%d", fd);
  FILE* f = fopen(path, "r");
  bool found = false;
  while (f && fgets(line, sizeof line, f))
    found |= strstr(line, "udmabuf") != nullptr;
  if (f) fclose(f);
  return found;
}

static native_handle_t* fromAidl(const NativeHandle& h) {
  native_handle_t* nh =
    innkeeper::nativeHandleCreate(int(h.fds.size()), int(h.ints.size()));
  for (size_t i = 0; i < h.fds.size(); i++) nh->data[i] = dup(h.fds[i].get());
  memcpy(&nh->data[h.fds.size()], h.ints.data(), h.ints.size() * sizeof(int));
  return nh;
}

static BufferDescriptorInfo desc(
  const char* name, int w, int h, PixelFormat f, int64_t usage
) {
  BufferDescriptorInfo d;
  d.name.fill(0);
  memcpy(d.name.data(), name, strlen(name));
  d.width = w;
  d.height = h;
  d.layerCount = 1;
  d.format = f;
  d.usage = static_cast<BufferUsage>(usage);
  d.reservedSize = 0;
  return d;
}

static void testAidl(const char* mapperPath) {
  const std::string name = std::string(IAllocator::descriptor) + "/default";
  CHECK(AServiceManager_isDeclared(name.c_str()));
  auto alloc = IAllocator::fromBinder(
    ndk::SpAIBinder(AServiceManager_waitForService(name.c_str()))
  );
  CHECK(alloc != nullptr);
  if (!alloc) return;
  int32_t ver = 0;
  alloc->getInterfaceVersion(&ver);
  std::string suffix;
  alloc->getIMapperLibrarySuffix(&suffix);
  printf("IAllocator V%d, mapper suffix '%s'\n", ver, suffix.c_str());
  CHECK(ver == 3);
  CHECK(suffix == "innkeeper");

  void* so = dlopen(mapperPath, RTLD_NOW | RTLD_LOCAL);
  if (!so) {
    fprintf(stderr, "dlopen %s: %s\n", mapperPath, dlerror());
    gFail++;
    return;
  }
  auto load = (AIMapper_Error (*)(AIMapper**))dlsym(so, "AIMapper_loadIMapper");
  CHECK(load && load(&gMapper) == AIMAPPER_ERROR_NONE);

  const int64_t cpu = int64_t(BufferUsage::CPU_READ_OFTEN) |
                      int64_t(BufferUsage::CPU_WRITE_OFTEN);
  struct Case {
    PixelFormat f;
    int w, h;
    int64_t usage;
  } cases[] = {
    {PixelFormat::RGBA_8888, 1280, 800,
     cpu | int64_t(BufferUsage::GPU_RENDER_TARGET) |
       int64_t(BufferUsage::COMPOSER_CLIENT_TARGET)},
    {PixelFormat::RGBX_8888, 333, 77, cpu},
    {PixelFormat::BGRA_8888, 64, 64, cpu | int64_t(BufferUsage::GPU_TEXTURE)},
    {PixelFormat::RGB_565, 100, 100, cpu},
    {PixelFormat::RGBA_FP16, 256, 256, cpu},
    {PixelFormat::RGBA_1010102, 256, 256, cpu},
    {PixelFormat::YCBCR_420_888, 640, 480,
     cpu | int64_t(BufferUsage::VIDEO_DECODER)},
    {PixelFormat::YV12, 640, 480, cpu},
    {PixelFormat::BLOB, 65536, 1, cpu | int64_t(BufferUsage::GPU_DATA_BUFFER)},
    {PixelFormat::IMPLEMENTATION_DEFINED, 320, 240,
     int64_t(BufferUsage::GPU_TEXTURE) |
       int64_t(BufferUsage::COMPOSER_OVERLAY)},
  };
  for (const auto& c : cases) {
    auto d = desc("gralloc_test", c.w, c.h, c.f, c.usage);
    bool supported = false;
    alloc->isSupported(d, &supported);
    CHECK(supported);
    AllocationResult r;
    auto st = alloc->allocate2(d, 2, &r);
    CHECK(st.isOk() && r.buffers.size() == 2);
    if (!st.isOk() || r.buffers.empty()) continue;
    native_handle_t* raw = fromAidl(r.buffers[0]);
    buffer_handle_t h = nullptr;
    CHECK(gMapper->v5.importBuffer(raw, &h) == AIMAPPER_ERROR_NONE);
    void* p = nullptr;
    CHECK(
      gMapper->v5.lock(h, uint64_t(cpu), ARect{0, 0, c.w, c.h}, -1, &p) ==
      AIMAPPER_ERROR_NONE
    );
    auto layouts = *get<StandardMetadataType::PLANE_LAYOUTS>(h);
    for (auto& pl : layouts)
      memset((uint8_t*)p + pl.offsetInBytes, 0xa5, pl.totalSizeInBytes);
    int rf;
    gMapper->v5.unlock(h, &rf);
    // Second import (as another process would) sees the bytes.
    buffer_handle_t h2;
    gMapper->v5.importBuffer(raw, &h2);
    void* p2;
    gMapper->v5.lock(h2, uint64_t(cpu), ARect{}, -1, &p2);
    auto& last = layouts.back();
    CHECK(
      ((uint8_t*)p2)[last.offsetInBytes + last.totalSizeInBytes - 1] == 0xa5
    );
    gMapper->v5.unlock(h2, &rf);
    auto fourcc = *get<StandardMetadataType::PIXEL_FORMAT_FOURCC>(h);
    printf(
      "  %-22s %4dx%-4d stride=%-5d planes=%zu fourcc=%.4s udmabuf=%s\n",
      toString(c.f).c_str(), c.w, c.h, r.stride, layouts.size(), (char*)&fourcc,
      isUdmabuf(raw->data[0]) ? "yes" : "no"
    );
    CHECK(*get<StandardMetadataType::WIDTH>(h) == uint64_t(c.w));
    CHECK(*get<StandardMetadataType::STRIDE>(h) == uint32_t(r.stride));
    CHECK(*get<StandardMetadataType::NAME>(h) == "gralloc_test");
    CHECK(
      *get<StandardMetadataType::BUFFER_ID>(h) ==
      *get<StandardMetadataType::BUFFER_ID>(h2)
    );
    gMapper->v5.freeBuffer(h);
    gMapper->v5.freeBuffer(h2);
    innkeeper::nativeHandleCloseAndDelete(raw);
  }
  // Errors are ServiceSpecific AllocationError codes.
  AllocationResult r;
  auto st =
    alloc->allocate2(desc("bad", 0, 10, PixelFormat::RGBA_8888, cpu), 1, &r);
  CHECK(
    st.getServiceSpecificError() == int32_t(AllocationError::BAD_DESCRIPTOR)
  );
  st = alloc->allocate2(
    desc(
      "bad", 10, 10, PixelFormat::RGBA_8888, int64_t(BufferUsage::PROTECTED)
    ),
    1, &r
  );
  CHECK(st.getServiceSpecificError() == int32_t(AllocationError::UNSUPPORTED));
}

static void testAHardwareBuffer() {
  AHardwareBuffer_Desc d = {};
  d.width = 300;
  d.height = 200;
  d.layers = 1;
  d.format = AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM;
  d.usage = AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN |
            AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN |
            AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE;
  CHECK(AHardwareBuffer_isSupported(&d) == 1);
  AHardwareBuffer* ahb = nullptr;
  int err = AHardwareBuffer_allocate(&d, &ahb);
  CHECK(err == 0 && ahb);
  if (!ahb) return;
  AHardwareBuffer_Desc out;
  AHardwareBuffer_describe(ahb, &out);
  printf("AHardwareBuffer 300x200 RGBA: stride=%u\n", out.stride);
  CHECK(out.stride == 320);  // 256-byte rows
  void* p = nullptr;
  CHECK(
    AHardwareBuffer_lock(
      ahb, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &p
    ) == 0
  );
  if (p) ((uint32_t*)p)[out.stride * 199 + 299] = 0x12345678;
  CHECK(AHardwareBuffer_unlock(ahb, nullptr) == 0);
  CHECK(
    AHardwareBuffer_lock(
      ahb, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, -1, nullptr, &p
    ) == 0
  );
  if (p) CHECK(((uint32_t*)p)[out.stride * 199 + 299] == 0x12345678);
  AHardwareBuffer_unlock(ahb, nullptr);
  uint64_t id = 0;
  CHECK(AHardwareBuffer_getId(ahb, &id) == 0 && id != 0);
  AHardwareBuffer_release(ahb);

  // YUV through lockPlanes.
  d.width = 640;
  d.height = 480;
  d.format = AHARDWAREBUFFER_FORMAT_Y8Cb8Cr8_420;
  d.usage = AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN |
            AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN;
  err = AHardwareBuffer_allocate(&d, &ahb);
  CHECK(err == 0);
  if (err) return;
  AHardwareBuffer_Planes planes = {};
  CHECK(
    AHardwareBuffer_lockPlanes(
      ahb, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &planes
    ) == 0
  );
  printf(
    "AHardwareBuffer 640x480 YUV420: %u planes, Y rowStride=%u, Cb "
    "pixelStride=%u "
    "(Cr-Cb=%td)\n",
    planes.planeCount, planes.planes[0].rowStride, planes.planes[1].pixelStride,
    (uint8_t*)planes.planes[2].data - (uint8_t*)planes.planes[1].data
  );
  CHECK(planes.planeCount == 3);
  CHECK(planes.planes[1].pixelStride == 2);  // NV12
  CHECK((uint8_t*)planes.planes[2].data - (uint8_t*)planes.planes[1].data == 1);
  AHardwareBuffer_unlock(ahb, nullptr);
  AHardwareBuffer_release(ahb);
}

int main(int argc, char** argv) {
  setvbuf(stdout, nullptr, _IONBF, 0);
  ABinderProcess_startThreadPool();
  testAidl(argc > 1 ? argv[1] : "/vendor/lib64/hw/mapper.innkeeper.so");
  testAHardwareBuffer();
  printf("%d checks, %d failures\n", gChecks, gFail);
  return gFail ? 1 : 0;
}
