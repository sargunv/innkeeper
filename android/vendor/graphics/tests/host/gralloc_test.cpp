// SPDX-License-Identifier: AGPL-3.0-or-later
// Host (glibc) tests for the buffer layout, allocation and the IMapper v5
// implementation. The mapper is dlopen'ed exactly like libui does it.
#include <string>
#include <vector>

#include <aidl/android/hardware/graphics/common/PlaneLayoutComponentType.h>
#include <android/hardware/graphics/mapper/IMapper.h>
#include <android/hardware/graphics/mapper/utils/IMapperMetadataTypes.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <linux/dma-buf.h>
#include <poll.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "innkeeper/buffer_alloc.h"
#include "innkeeper/gralloc_handle.h"
#include "innkeeper/layout.h"
#include "innkeeper/native_handle.h"

using namespace innkeeper;
using namespace ::android::hardware::graphics::mapper;
using ::aidl::android::hardware::graphics::common::PlaneLayoutComponentType;

void innkeeper_host_assert(
  const char* file, int line, const char* msg
) noexcept {
  fprintf(stderr, "%s:%d: %s\n", file, line, msg);
  abort();
}

static int gFailures = 0, gChecks = 0;
#define CHECK(cond)                                                   \
  do {                                                                \
    gChecks++;                                                        \
    if (!(cond)) {                                                    \
      gFailures++;                                                    \
      fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
    }                                                                 \
  } while (0)
#define CHECK_EQ(a, b)                                                       \
  do {                                                                       \
    gChecks++;                                                               \
    auto _a = (a);                                                           \
    auto _b = (b);                                                           \
    if (!(_a == _b)) {                                                       \
      gFailures++;                                                           \
      fprintf(                                                               \
        stderr, "FAIL %s:%d: %s == %s (%lld vs %lld)\n", __FILE__, __LINE__, \
        #a, #b, (long long)_a, (long long)_b                                 \
      );                                                                     \
    }                                                                        \
  } while (0)

static AIMapper* gMapper;

template <StandardMetadataType T>
static auto get(buffer_handle_t h)
  -> decltype(StandardMetadata<T>::value::decode(nullptr, 0)) {
  std::vector<uint8_t> buf(16);
  int32_t n =
    gMapper->v5.getStandardMetadata(h, int64_t(T), buf.data(), buf.size());
  if (n < 0) return std::nullopt;
  if (size_t(n) > buf.size()) {
    buf.resize(n);
    n = gMapper->v5.getStandardMetadata(h, int64_t(T), buf.data(), buf.size());
  }
  return StandardMetadata<T>::value::decode(buf.data(), n);
}

template <StandardMetadataType T>
static AIMapper_Error set(
  buffer_handle_t h, const typename StandardMetadata<T>::value_type& v
) {
  std::vector<uint8_t> buf(StandardMetadata<T>::value::encode(v, nullptr, 0));
  (void)StandardMetadata<T>::value::encode(v, buf.data(), buf.size());
  return gMapper->v5.setStandardMetadata(h, int64_t(T), buf.data(), buf.size());
}

static void testLayouts() {
  Layout l;
  CHECK(computeLayout(fmt::RGBA_8888, 1280, 800, 1, 0, &l));
  CHECK_EQ(l.fourcc, fourcc('A', 'B', '2', '4'));
  CHECK_EQ(l.stridePixels, 1280u);
  CHECK_EQ(l.planes[0].stride, 5120u);
  CHECK_EQ(l.layerSize, 5120ull * 800);

  CHECK(computeLayout(fmt::RGBA_8888, 100, 10, 1, 0, &l));
  CHECK_EQ(l.stridePixels, 128u);  // 256-byte rows

  CHECK(computeLayout(fmt::RGB_565, 100, 10, 1, 0, &l));
  CHECK_EQ(l.planes[0].stride, 256u);
  CHECK_EQ(l.stridePixels, 128u);

  CHECK(computeLayout(fmt::RGB_888, 10, 10, 1, 0, &l));
  CHECK_EQ(l.planes[0].stride % 256, 0u);
  CHECK_EQ(l.planes[0].stride % 3, 0u);

  CHECK(computeLayout(fmt::RGBA_FP16, 33, 2, 1, 0, &l));
  CHECK_EQ(l.planes[0].stride, 512u);
  CHECK_EQ(l.stridePixels, 64u);

  CHECK(computeLayout(fmt::BLOB, 4 << 20, 1, 1, 0, &l));  // 4 MiB data buffer
  CHECK(computeLayout(fmt::BLOB, 1000, 1, 1, 0, &l));
  CHECK_EQ(l.planes[0].stride, 1000u);
  CHECK_EQ(l.layerSize, 4096ull);

  CHECK(computeLayout(fmt::YV12, 640, 480, 1, 0, &l));
  CHECK_EQ(l.numPlanes, 3u);
  CHECK_EQ(
    l.planes[0].stride, 1024u
  );  // 512-aligned so chroma rows stay 256-aligned
  CHECK_EQ(l.planes[1].stride, 512u);           // align(y_stride/2, 16)
  CHECK_EQ(l.planes[1].offset, 1024ull * 480);  // Cr
  CHECK_EQ(l.planes[2].offset, 1024ull * 480 + 512ull * 240);  // Cb
  CHECK_EQ(l.stridePixels, 1024u);

  CHECK(computeLayout(fmt::YCBCR_420_888, 641, 481, 1, 0, &l));
  CHECK_EQ(l.fourcc, fourcc('N', 'V', '1', '2'));
  CHECK_EQ(l.planes[1].offset, uint64_t(l.planes[0].stride) * 481);
  CHECK_EQ(l.planes[1].heightInSamples, 241u);
  CHECK_EQ(l.planes[1].widthInSamples, 321u);

  CHECK(computeLayout(
    fmt::IMPLEMENTATION_DEFINED, 64, 64, 1, usage::GPU_TEXTURE, &l
  ));
  CHECK_EQ(l.resolvedFormat, fmt::RGBA_8888);
  CHECK(computeLayout(
    fmt::IMPLEMENTATION_DEFINED, 64, 64, 1, usage::VIDEO_DECODER, &l
  ));
  CHECK_EQ(l.resolvedFormat, fmt::YCBCR_420_888);

  CHECK(computeLayout(fmt::RGBA_8888, 64, 64, 3, 0, &l));
  CHECK_EQ(l.pixelBytes, 3 * l.layerSize);

  CHECK(!computeLayout(0x7777, 64, 64, 1, 0, &l));
  CHECK(!computeLayout(fmt::RGBA_8888, 0, 64, 1, 0, &l));
}

static bool isDmabuf(int fd) {
  char path[64];
  snprintf(path, sizeof path, "/proc/self/fdinfo/%d", fd);
  FILE* f = fopen(path, "r");
  if (!f) return false;
  char line[256];
  bool found = false;
  while (fgets(line, sizeof line, f)) {
    if (strstr(line, "exp_name:") && strstr(line, "udmabuf")) found = true;
  }
  fclose(f);
  return found;
}

static native_handle_t* alloc(
  int32_t format, uint32_t w, uint32_t h, uint64_t u, const char* name = "test",
  uint64_t reserved = 0
) {
  native_handle_t* nh = nullptr;
  AllocRequest req{name, w, h, 1, format, u, reserved};
  AllocStatus st = allocateBuffer(req, &nh);
  CHECK(st == AllocStatus::Ok);
  return nh;
}

// Verify actual VM mappings, including udmabuf-backed GPU allocations.
static bool mapsPixels(int fd) {
  struct stat st{};
  if (fstat(fd, &st) != 0) abort();
  FILE* maps = fopen("/proc/self/maps", "r");
  if (!maps) abort();
  char line[1024];
  bool found = false;
  while (fgets(line, sizeof(line), maps)) {
    unsigned long long offset = 0, inode = 0;
    if (
      sscanf(line, "%*x-%*x %*s %llx %*s %llu", &offset, &inode) == 2 &&
      inode == static_cast<unsigned long long>(st.st_ino) && offset == 0
    )
      found = true;
  }
  fclose(maps);
  return found;
}

static void testMapper() {
  const uint64_t u = 0x33 | usage::GPU_TEXTURE | usage::GPU_RENDER_TARGET;
  native_handle_t* raw = alloc(fmt::RGBA_8888, 320, 200, u, "host-test", 100);
  if (!raw) return;
  const BufferHandle* bh = asBufferHandle(raw);
  CHECK(bh != nullptr);
  printf(
    "  allocated 320x200 RGBA_8888: fd=%d dmabuf=%d (fdinfo says %s) "
    "size=%llu\n",
    bh->fd, !!(bh->flags & kFlagDmabuf),
    isDmabuf(bh->fd) ? "udmabuf" : "not dmabuf",
    (unsigned long long)bh->totalSize.get()
  );
  CHECK(isDmabuf(bh->fd) == !!(bh->flags & kFlagDmabuf));
  // Pixels only; metadata and the 100-byte reserved region have their own fd.
  CHECK_EQ(uint64_t(lseek(bh->fd, 0, SEEK_END)), bh->totalSize.get());
  CHECK_EQ(lseek(bh->metadataFd, 0, SEEK_END), off_t(2 * kPageSize));

  buffer_handle_t a = nullptr, b = nullptr;
  CHECK_EQ(gMapper->v5.importBuffer(raw, &a), AIMAPPER_ERROR_NONE);
  CHECK_EQ(gMapper->v5.importBuffer(raw, &b), AIMAPPER_ERROR_NONE);
  CHECK(a != b && a != raw);
  CHECK(!mapsPixels(bh->fd));

  uint32_t nfds = 0, nints = 0;
  CHECK_EQ(gMapper->v5.getTransportSize(a, &nfds, &nints), AIMAPPER_ERROR_NONE);
  CHECK_EQ(nfds, 2u);
  CHECK_EQ(int(nints), raw->numInts);

  // Write through one import, read through the other (same pages).
  void* p = nullptr;
  CHECK_EQ(
    gMapper->v5.lock(a, 0x30, ARect{0, 0, 320, 200}, -1, &p),
    AIMAPPER_ERROR_NONE
  );
  CHECK(mapsPixels(bh->fd));
  auto* px = static_cast<uint32_t*>(p);
  const uint32_t stride = *get<StandardMetadataType::STRIDE>(a);
  for (uint32_t y = 0; y < 200; y++)
    for (uint32_t x = 0; x < 320; x++)
      px[y * stride + x] = 0xff000000u | (y << 8) | x;
  int rf = 0;
  CHECK_EQ(gMapper->v5.unlock(a, &rf), AIMAPPER_ERROR_NONE);
  CHECK_EQ(rf, -1);
  void* q = nullptr;
  CHECK_EQ(
    gMapper->v5.lock(b, 0x3, ARect{0, 0, 0, 0}, -1, &q), AIMAPPER_ERROR_NONE
  );
  CHECK_EQ(
    static_cast<uint32_t*>(q)[199 * stride + 319],
    0xff000000u | (199u << 8) | 319u
  );
  CHECK_EQ(gMapper->v5.unlock(b, &rf), AIMAPPER_ERROR_NONE);

  // A child process imports its inherited copy and sees/changes the same
  // memory.
  pid_t pid = fork();
  if (pid == 0) {
    buffer_handle_t c;
    if (gMapper->v5.importBuffer(raw, &c) != AIMAPPER_ERROR_NONE) _exit(2);
    void* cp;
    gMapper->v5.lock(c, 0x33, ARect{}, -1, &cp);
    bool ok = static_cast<uint32_t*>(cp)[10 * stride + 10] ==
              (0xff000000u | (10 << 8) | 10);
    static_cast<uint32_t*>(cp)[0] = 0xdeadbeef;
    _exit(ok ? 0 : 1);
  }
  int status = 0;
  waitpid(pid, &status, 0);
  CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
  CHECK_EQ(static_cast<uint32_t*>(q)[0], 0xdeadbeefu);

  // Bad lock arguments.
  CHECK_EQ(gMapper->v5.lock(a, 0, ARect{}, -1, &p), AIMAPPER_ERROR_BAD_VALUE);
  CHECK_EQ(
    gMapper->v5.lock(a, 0x3 | usage::GPU_TEXTURE, ARect{}, -1, &p),
    AIMAPPER_ERROR_BAD_VALUE
  );
  CHECK_EQ(
    gMapper->v5.lock(a, 0x3, ARect{0, 0, 321, 200}, -1, &p),
    AIMAPPER_ERROR_BAD_VALUE
  );

  // Immutable metadata.
  using S = StandardMetadataType;
  CHECK_EQ(*get<S::WIDTH>(a), 320u);
  CHECK_EQ(*get<S::HEIGHT>(a), 200u);
  CHECK_EQ(*get<S::LAYER_COUNT>(a), 1u);
  CHECK(*get<S::NAME>(a) == "host-test");
  CHECK_EQ(*get<S::BUFFER_ID>(a), *get<S::BUFFER_ID>(b));
  CHECK(*get<S::PIXEL_FORMAT_REQUESTED>(a) == PixelFormat::RGBA_8888);
  CHECK_EQ(*get<S::PIXEL_FORMAT_FOURCC>(a), fourcc('A', 'B', '2', '4'));
  CHECK_EQ(*get<S::PIXEL_FORMAT_MODIFIER>(a), 0u);
  CHECK_EQ(uint64_t(*get<S::USAGE>(a)), u);
  CHECK_EQ(*get<S::ALLOCATION_SIZE>(a), bh->totalSize.get());
  CHECK_EQ(*get<S::PROTECTED_CONTENT>(a), 0u);
  CHECK_EQ(stride, 320u);
  CHECK(
    get<S::COMPRESSION>(a)->name ==
    "android.hardware.graphics.common.Compression"
  );
  CHECK(get<S::INTERLACED>(a).has_value());
  CHECK(get<S::CHROMA_SITING>(a).has_value());
  auto crop = *get<S::CROP>(a);
  CHECK_EQ(crop.size(), 1u);
  CHECK_EQ(crop[0].right, 320);
  auto layouts = *get<S::PLANE_LAYOUTS>(a);
  CHECK_EQ(layouts.size(), 1u);
  CHECK_EQ(layouts[0].components.size(), 4u);
  CHECK_EQ(
    layouts[0].components[0].type.value, int64_t(PlaneLayoutComponentType::R)
  );
  CHECK_EQ(
    layouts[0].components[3].type.value, int64_t(PlaneLayoutComponentType::A)
  );
  CHECK_EQ(layouts[0].components[3].offsetInBits, 24);
  CHECK_EQ(layouts[0].sampleIncrementInBits, 32);
  CHECK_EQ(layouts[0].strideInBytes, 1280);
  CHECK_EQ(layouts[0].widthInSamples, 320);
  CHECK_EQ(layouts[0].totalSizeInBytes, 1280 * 200);
  CHECK_EQ(set<S::WIDTH>(a, 5), AIMAPPER_ERROR_BAD_VALUE);

  // Mutable metadata is shared between imports (lives in the buffer).
  CHECK(*get<S::DATASPACE>(a) == Dataspace::UNKNOWN);
  CHECK_EQ(set<S::DATASPACE>(a, Dataspace::SRGB), AIMAPPER_ERROR_NONE);
  CHECK(*get<S::DATASPACE>(b) == Dataspace::SRGB);
  CHECK_EQ(
    set<S::BLEND_MODE>(a, BlendMode::PREMULTIPLIED), AIMAPPER_ERROR_NONE
  );
  CHECK(*get<S::BLEND_MODE>(b) == BlendMode::PREMULTIPLIED);
  CHECK(!get<S::SMPTE2086>(b)->has_value());
  Smpte2086 hdr{{0.68f, 0.32f}, {0.265f, 0.69f},
                {0.15f, 0.06f}, {0.3127f, 0.329f},
                1000,           0.01f};
  CHECK_EQ(set<S::SMPTE2086>(a, hdr), AIMAPPER_ERROR_NONE);
  auto hdr2 = *get<S::SMPTE2086>(b);
  CHECK(
    hdr2.has_value() && hdr2->maxLuminance == 1000.f &&
    hdr2->primaryGreen.y == 0.69f
  );
  CHECK_EQ(set<S::SMPTE2086>(a, std::nullopt), AIMAPPER_ERROR_NONE);
  CHECK(!get<S::SMPTE2086>(b)->has_value());
  CHECK_EQ(set<S::CTA861_3>(a, Cta861_3{500.f, 200.f}), AIMAPPER_ERROR_NONE);
  CHECK((*get<S::CTA861_3>(b))->maxFrameAverageLightLevel == 200.f);
  std::vector<uint8_t> blob = {1, 2, 3, 4, 5};
  CHECK_EQ(set<S::SMPTE2094_40>(a, blob), AIMAPPER_ERROR_NONE);
  CHECK(**get<S::SMPTE2094_40>(b) == blob);

  // getMetadata with the standard name is the same as getStandardMetadata.
  uint8_t buf[256];
  int32_t n = gMapper->v5.getMetadata(
    a,
    {"android.hardware.graphics.common.StandardMetadataType",
     int64_t(S::WIDTH)},
    buf, sizeof buf
  );
  CHECK(n > 0 && *StandardMetadata<S::WIDTH>::value::decode(buf, n) == 320u);
  CHECK_EQ(
    gMapper->v5.getMetadata(a, {"vendor.foo", 1}, buf, sizeof buf),
    -AIMAPPER_ERROR_UNSUPPORTED
  );

  // Reserved region.
  void* rr = nullptr;
  uint64_t rs = 0;
  CHECK_EQ(gMapper->v5.getReservedRegion(a, &rr, &rs), AIMAPPER_ERROR_NONE);
  CHECK(rr != nullptr);
  CHECK_EQ(rs, 100u);
  memset(rr, 0x5a, 100);

  // listSupportedMetadataTypes / dump.
  const AIMapper_MetadataTypeDescription* list;
  size_t count;
  CHECK_EQ(
    gMapper->v5.listSupportedMetadataTypes(&list, &count), AIMAPPER_ERROR_NONE
  );
  CHECK(count >= 20);
  int dumped = 0;
  gMapper->v5.dumpBuffer(
    a,
    [](void* ctx, AIMapper_MetadataType, const void*, size_t) { ++*(int*)ctx; },
    &dumped
  );
  CHECK(dumped >= 20);

  // Lock with a signalled sync_file as acquire fence (exported from the
  // dma-buf).
  if (bh->flags & kFlagDmabuf) {
    struct dma_buf_export_sync_file ex = {DMA_BUF_SYNC_READ, -1};
    int r = ioctl(bh->fd, DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &ex);
    CHECK(r == 0 && ex.fd >= 0);
    if (r == 0) {
      struct pollfd pfd = {ex.fd, POLLIN, 0};
      CHECK_EQ(poll(&pfd, 1, 0), 1);  // already signalled
      CHECK_EQ(
        gMapper->v5.lock(a, 0x3, ARect{}, ex.fd, &p), AIMAPPER_ERROR_NONE
      );
      CHECK_EQ(gMapper->v5.unlock(a, &rf), AIMAPPER_ERROR_NONE);
      CHECK(
        fcntl(ex.fd, F_GETFD) == -1
      );  // mapper took ownership and closed it
    }
  }

  CHECK_EQ(gMapper->v5.freeBuffer(a), AIMAPPER_ERROR_NONE);
  CHECK_EQ(gMapper->v5.freeBuffer(a), AIMAPPER_ERROR_BAD_BUFFER);
  CHECK_EQ(gMapper->v5.freeBuffer(b), AIMAPPER_ERROR_NONE);

  // Rejects foreign handles.
  native_handle_t* bogus = nativeHandleCreate(1, 3);
  bogus->data[0] = -1;
  buffer_handle_t out;
  CHECK_EQ(gMapper->v5.importBuffer(bogus, &out), AIMAPPER_ERROR_BAD_BUFFER);
  nativeHandleDelete(bogus);
  nativeHandleCloseAndDelete(raw);
}

static void testAllFormats() {
  const int32_t formats[] = {
    fmt::RGBA_8888,
    fmt::RGBX_8888,
    fmt::BGRA_8888,
    fmt::RGB_888,
    fmt::RGB_565,
    fmt::RGBA_FP16,
    fmt::RGBA_1010102,
    fmt::R_8,
    fmt::BLOB,
    fmt::Y8,
    fmt::Y16,
    fmt::RAW16,
    fmt::YCBCR_420_888,
    fmt::YCRCB_420_SP,
    fmt::YV12,
    fmt::YCBCR_P010,
    fmt::IMPLEMENTATION_DEFINED
  };
  for (int32_t f : formats) {
    uint32_t h = f == fmt::BLOB ? 1 : 97;
    native_handle_t* raw = alloc(f, 131, h, 0x33, formatName(f));
    if (!raw) continue;
    buffer_handle_t a;
    CHECK_EQ(gMapper->v5.importBuffer(raw, &a), AIMAPPER_ERROR_NONE);
    auto layouts = *get<StandardMetadataType::PLANE_LAYOUTS>(a);
    const BufferHandle* bh = asBufferHandle(raw);
    CHECK_EQ(layouts.size(), size_t(bh->numPlanes));
    void* p;
    CHECK_EQ(gMapper->v5.lock(a, 0x33, ARect{}, -1, &p), AIMAPPER_ERROR_NONE);
    // Touch the last byte of every plane: must be mapped.
    for (auto& pl : layouts) {
      CHECK(
        pl.offsetInBytes + pl.totalSizeInBytes <= int64_t(bh->layerSize.get())
      );
      if (f != fmt::BLOB) CHECK(pl.strideInBytes % 256 == 0);
      CHECK(
        pl.strideInBytes * 8 >= pl.widthInSamples * pl.sampleIncrementInBits
      );
      static_cast<uint8_t*>(p)[pl.offsetInBytes + pl.totalSizeInBytes - 1] =
        0x42;
    }
    int rf;
    gMapper->v5.unlock(a, &rf);
    printf(
      "  %-22s fourcc=%.4s planes=%zu stride0=%lldB STRIDE=%u size=%llu\n",
      formatName(f), (const char*)&bh->fourcc, layouts.size(),
      (long long)layouts[0].strideInBytes,
      *get<StandardMetadataType::STRIDE>(a),
      (unsigned long long)bh->totalSize.get()
    );
    gMapper->v5.freeBuffer(a);
    nativeHandleCloseAndDelete(raw);
  }
  // Large (> udmabuf size limit) still works via memfd fallback.
  native_handle_t* big = alloc(fmt::RGBA_FP16, 3840, 2160, 0x33, "big");
  if (big) {
    const BufferHandle* bh = asBufferHandle(big);
    printf(
      "  3840x2160 FP16 (%llu MiB): dmabuf=%d\n",
      (unsigned long long)(bh->totalSize.get() >> 20),
      !!(bh->flags & kFlagDmabuf)
    );
    buffer_handle_t a;
    CHECK_EQ(gMapper->v5.importBuffer(big, &a), AIMAPPER_ERROR_NONE);
    gMapper->v5.freeBuffer(a);
    nativeHandleCloseAndDelete(big);
  }
  // Over the udmabuf size limit: CPU-only falls back to memfd, GPU usage fails.
  if (udmabufAvailable()) {
    native_handle_t* nh = nullptr;
    CHECK(
      allocateBuffer(
        {"huge-cpu", 8192, 4096, 1, fmt::RGBA_8888, 0x33, 0}, &nh
      ) == AllocStatus::Ok
    );
    if (nh) {
      CHECK(!(asBufferHandle(nh)->flags & kFlagDmabuf));
      nativeHandleCloseAndDelete(nh);
    }
    CHECK(
      allocateBuffer(
        {"huge-gpu", 8192, 4096, 1, fmt::RGBA_8888, usage::GPU_TEXTURE, 0}, &nh
      ) == AllocStatus::NoResources
    );
  }
  native_handle_t* nh = nullptr;
  CHECK(
    allocateBuffer(
      {"p", 64, 64, 1, fmt::RGBA_8888, usage::PROTECTED, 0}, &nh
    ) == AllocStatus::Unsupported
  );
  CHECK(
    allocateBuffer({"z", 0, 64, 1, fmt::RGBA_8888, 0, 0}, &nh) ==
    AllocStatus::BadDescriptor
  );
}

static int gpuAllocations;
static uint64_t gpuPlaneOffset;  // where the fixture driver puts plane 0
static bool gpuOffsetCovered;    // and whether its allocation covers that
static GpuBuffer testGpuAllocation(const AllocRequest& req, Layout* layout) {
  ++gpuAllocations;
  layout->planes[0].offset = gpuPlaneOffset;
  layout->planes[0].stride *= 2;
  layout->stridePixels *= 2;
  layout->planes[0].size =
    uint64_t(layout->planes[0].stride) * alignUp(req.height, 32);
  layout->layerSize = alignUp(
    layout->planes[0].size + (gpuOffsetCovered ? gpuPlaneOffset : 0), kPageSize
  );
  layout->pixelBytes = layout->layerSize;
  // Exactly the image, as a driver's dedicated allocation is.
  int fd = memfd_create("gpu-layout-fixture", MFD_CLOEXEC);
  if (fd < 0 || ftruncate(fd, off_t(layout->pixelBytes))) abort();
  return {fd, 0x0100000000000002};
}

static void testGpuLayout() {
  for (uint64_t cpu :
       {uint64_t(0), usage::CPU_READ_MASK, usage::CPU_WRITE_MASK,
        usage::COMPOSER_CURSOR, usage::VIDEO_ENCODER}) {
    const int before = gpuAllocations;
    native_handle_t* raw = nullptr;
    CHECK_EQ(
      allocateBuffer(
        {"gpu-layout", 100, 50, 1, fmt::RGBA_8888,
         usage::GPU_RENDER_TARGET | cpu, 64},
        &raw, testGpuAllocation
      ),
      AllocStatus::Ok
    );
    if (!raw) continue;
    CHECK_EQ(gpuAllocations, before + (cpu == 0));
    buffer_handle_t imported = nullptr;
    CHECK_EQ(gMapper->v5.importBuffer(raw, &imported), AIMAPPER_ERROR_NONE);
    if (imported) {
      CHECK_EQ(
        *get<StandardMetadataType::PIXEL_FORMAT_MODIFIER>(imported),
        cpu ? uint64_t(0) : uint64_t(0x0100000000000002)
      );
      CHECK_EQ(*get<StandardMetadataType::STRIDE>(imported), cpu ? 128u : 256u);
      CHECK_EQ(
        get<StandardMetadataType::PLANE_LAYOUTS>(imported)->at(0).strideInBytes,
        cpu ? 512 : 1024
      );
      void* region = nullptr;
      uint64_t size = 0;
      CHECK_EQ(
        gMapper->v5.getReservedRegion(imported, &region, &size),
        AIMAPPER_ERROR_NONE
      );
      CHECK(region && size == 64);
      if (region) memset(region, 0x5a, size);
      gMapper->v5.freeBuffer(imported);
    }
    nativeHandleCloseAndDelete(raw);
  }
  // A plane at a nonzero offset stays tiled when the allocation covers it,
  // and falls back to LINEAR rather than producing an unimportable handle.
  for (bool covered : {true, false}) {
    gpuPlaneOffset = kPageSize;
    gpuOffsetCovered = covered;
    native_handle_t* raw = nullptr;
    CHECK_EQ(
      allocateBuffer(
        {"gpu-offset", 100, 50, 1, fmt::RGBA_8888, usage::GPU_RENDER_TARGET, 0},
        &raw, testGpuAllocation
      ),
      AllocStatus::Ok
    );
    if (!raw) continue;
    buffer_handle_t imported = nullptr;
    CHECK_EQ(gMapper->v5.importBuffer(raw, &imported), AIMAPPER_ERROR_NONE);
    if (imported) {
      CHECK_EQ(
        *get<StandardMetadataType::PIXEL_FORMAT_MODIFIER>(imported),
        covered ? uint64_t(0x0100000000000002) : uint64_t(0)
      );
      CHECK_EQ(
        get<StandardMetadataType::PLANE_LAYOUTS>(imported)->at(0).offsetInBytes,
        covered ? int64_t(kPageSize) : int64_t(0)
      );
      gMapper->v5.freeBuffer(imported);
    }
    nativeHandleCloseAndDelete(raw);
  }
  gpuPlaneOffset = 0;
}

int main(int argc, char** argv) {
  const char* lib = argc > 1 ? argv[1] : "./mapper.innkeeper.so";
  void* so = dlopen(lib, RTLD_NOW | RTLD_LOCAL);
  if (!so) {
    fprintf(stderr, "dlopen %s: %s\n", lib, dlerror());
    return 1;
  }
  auto load = (AIMapper_Error (*)(AIMapper**))dlsym(so, "AIMapper_loadIMapper");
  auto ver = (uint32_t*)dlsym(so, "ANDROID_HAL_STABLEC_VERSION");
  if (!load || !ver) {
    fprintf(stderr, "missing exports\n");
    return 1;
  }
  CHECK_EQ(*ver, uint32_t(AIMAPPER_VERSION_5));
  CHECK_EQ(load(&gMapper), AIMAPPER_ERROR_NONE);
  CHECK_EQ(gMapper->version, AIMAPPER_VERSION_5);

  if (getenv("INNKEEPER_UDMABUF")) {
    // Run with INNKEEPER_UDMABUF=/nonexistent: CPU buffers fall back to
    // memfd, GPU/composer buffers are refused.
    printf("no-udmabuf mode\n");
    native_handle_t* nh = nullptr;
    CHECK(
      allocateBuffer({"cpu", 64, 64, 1, fmt::RGBA_8888, 0x33, 0}, &nh) ==
      AllocStatus::Ok
    );
    if (nh) {
      CHECK(!(asBufferHandle(nh)->flags & kFlagDmabuf));
      buffer_handle_t a;
      CHECK_EQ(gMapper->v5.importBuffer(nh, &a), AIMAPPER_ERROR_NONE);
      gMapper->v5.freeBuffer(a);
      nativeHandleCloseAndDelete(nh);
    }
    CHECK(
      checkRequest({"gpu", 64, 64, 1, fmt::RGBA_8888, usage::GPU_TEXTURE, 0}) ==
      AllocStatus::Unsupported
    );
    CHECK(
      checkRequest(
        {"ct", 64, 64, 1, fmt::RGBA_8888, usage::COMPOSER_CLIENT_TARGET, 0}
      ) == AllocStatus::Unsupported
    );
    printf("%d checks, %d failures\n", gChecks, gFailures);
    return gFailures ? 1 : 0;
  }
  printf("layouts\n");
  testLayouts();
  printf("mapper\n");
  testMapper();
  printf("GPU layout and CPU usage policy\n");
  testGpuLayout();
  printf("formats\n");
  testAllFormats();
  printf("%d checks, %d failures\n", gChecks, gFailures);
  return gFailures ? 1 : 0;
}
