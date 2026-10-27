// SPDX-License-Identifier: AGPL-3.0-or-later
// IMapper v5 (stable-C) for linuxapk buffers. Built as
// /vendor/lib64/hw/mapper.linuxapk.so and dlopen'ed by libui (Gralloc5) and
// any other gralloc user in the process.
#define LOG_TAG "linuxapk-mapper"

#include <cinttypes>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

#include <aidl/android/hardware/graphics/common/ChromaSiting.h>
#include <aidl/android/hardware/graphics/common/Compression.h>
#include <aidl/android/hardware/graphics/common/Interlaced.h>
#include <android/hardware/graphics/mapper/IMapper.h>
#include <android/hardware/graphics/mapper/utils/IMapperMetadataTypes.h>
#include <errno.h>
#include <poll.h>
#include <stddef.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include "linuxapk/gralloc_handle.h"
#include "linuxapk/log.h"
#include "linuxapk/native_handle.h"

using namespace ::android::hardware::graphics::mapper;
using ::aidl::android::hardware::graphics::common::ChromaSiting;
using ::aidl::android::hardware::graphics::common::Compression;
using ::aidl::android::hardware::graphics::common::Interlaced;
using AidlPlaneLayout =
  ::aidl::android::hardware::graphics::common::PlaneLayout;

namespace linuxapk {
namespace {

constexpr const char* kStandardName =
  "android.hardware.graphics.common.StandardMetadataType";

struct Imported {
  native_handle_t* handle =
    nullptr;  // our clone; this pointer is the imported handle
  const BufferHandle* bh = nullptr;
  uint8_t* base = nullptr;
  size_t mapSize = 0;

  SharedMetadata* md() const {
    return reinterpret_cast<SharedMetadata*>(base + bh->metadataOffset.get());
  }
};

class Registry {
 public:
  Imported* find(buffer_handle_t h) {
    std::lock_guard lock(mMutex);
    auto it = mBuffers.find(h);
    return it == mBuffers.end() ? nullptr : it->second.get();
  }
  void add(std::unique_ptr<Imported> b) {
    std::lock_guard lock(mMutex);
    mBuffers.emplace(b->handle, std::move(b));
  }
  std::unique_ptr<Imported> remove(buffer_handle_t h) {
    std::lock_guard lock(mMutex);
    auto it = mBuffers.find(h);
    if (it == mBuffers.end()) return nullptr;
    auto b = std::move(it->second);
    mBuffers.erase(it);
    return b;
  }
  template <typename F>
  void forEach(F&& f) {
    std::lock_guard lock(mMutex);
    for (auto& [_, b] : mBuffers) f(*b);
  }

 private:
  std::mutex mMutex;
  std::unordered_map<buffer_handle_t, std::unique_ptr<Imported>> mBuffers;
};

Registry& registry() {
  static Registry* r =
    new Registry();  // never destroyed: used from any thread at exit
  return *r;
}

void waitFence(int fence) {
  if (fence < 0) return;
  struct pollfd p = {fence, POLLIN, 0};
  int ret;
  do {
    ret = poll(&p, 1, 3000);
  } while (ret < 0 && (errno == EINTR || errno == EAGAIN));
  if (ret == 0)
    LOGW("acquire fence %d not signalled after 3s; proceeding", fence);
  close(fence);
}

uint64_t fdSize(int fd) {
  off_t end = lseek(fd, 0, SEEK_END);
  return end < 0 ? 0 : uint64_t(end);
}

// ---- AIMapperV5 ----

AIMapper_Error importBuffer(
  const native_handle_t* handle, buffer_handle_t* outBufferHandle
) {
  const BufferHandle* in = asBufferHandle(handle);
  if (!in) return AIMAPPER_ERROR_BAD_BUFFER;
  const uint64_t total = in->totalSize.get();
  const uint64_t mdEnd = in->metadataOffset.get() + kPageSize;
  const uint64_t resEnd = in->reservedOffset.get() + in->reservedSize.get();
  if (
    in->numPlanes == 0 || in->numPlanes > kMaxPlanes || mdEnd > total ||
    resEnd > total ||
    in->layerSize.get() * in->layerCount > in->metadataOffset.get()
  ) {
    LOGE("importBuffer: inconsistent handle");
    return AIMAPPER_ERROR_BAD_BUFFER;
  }
  for (uint32_t i = 0; i < in->numPlanes; i++) {
    if (
      in->planes[i].offset.get() + in->planes[i].size.get() >
      in->layerSize.get()
    ) {
      return AIMAPPER_ERROR_BAD_BUFFER;
    }
  }
  if (fdSize(in->fd) < total) {
    LOGE("importBuffer: fd smaller than handle claims");
    return AIMAPPER_ERROR_BAD_BUFFER;
  }

  native_handle_t* clone = nativeHandleClone(handle);
  if (!clone) return AIMAPPER_ERROR_NO_RESOURCES;
  auto b = std::make_unique<Imported>();
  b->handle = clone;
  b->bh = reinterpret_cast<const BufferHandle*>(clone);
  b->mapSize = size_t(total);
  void* p =
    mmap(nullptr, b->mapSize, PROT_READ | PROT_WRITE, MAP_SHARED, b->bh->fd, 0);
  if (p == MAP_FAILED) {
    LOGE("importBuffer: mmap(%zu): %s", b->mapSize, strerror(errno));
    nativeHandleCloseAndDelete(clone);
    return AIMAPPER_ERROR_NO_RESOURCES;
  }
  b->base = static_cast<uint8_t*>(p);
  if (b->md()->magic != SharedMetadata::kMagic) {
    LOGE("importBuffer: bad metadata magic");
    munmap(p, b->mapSize);
    nativeHandleCloseAndDelete(clone);
    return AIMAPPER_ERROR_BAD_BUFFER;
  }
  *outBufferHandle = clone;
  registry().add(std::move(b));
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error freeBuffer(buffer_handle_t buffer) {
  auto b = registry().remove(buffer);
  if (!b) return AIMAPPER_ERROR_BAD_BUFFER;
  munmap(b->base, b->mapSize);
  nativeHandleCloseAndDelete(b->handle);
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error getTransportSize(
  buffer_handle_t buffer, uint32_t* outNumFds, uint32_t* outNumInts
) {
  if (!registry().find(buffer)) return AIMAPPER_ERROR_BAD_BUFFER;
  *outNumFds = kHandleNumFds;
  *outNumInts = kHandleNumInts;
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error lock(
  buffer_handle_t buffer, uint64_t cpuUsage, ARect region, int acquireFence,
  void** outData
) {
  Imported* b = registry().find(buffer);
  if (!b) {
    if (acquireFence >= 0) close(acquireFence);
    return AIMAPPER_ERROR_BAD_BUFFER;
  }
  const uint64_t cpuMask = usage::CPU_READ_MASK | usage::CPU_WRITE_MASK;
  const bool wholeBuffer = region.left == 0 && region.top == 0 &&
                           region.right == 0 && region.bottom == 0;
  const bool regionOk =
    wholeBuffer ||
    (region.left >= 0 && region.top >= 0 && region.left <= region.right &&
     region.top <= region.bottom && region.right <= int32_t(b->bh->width) &&
     region.bottom <= int32_t(b->bh->height));
  if (cpuUsage == 0 || (cpuUsage & ~cpuMask) || !regionOk) {
    if (acquireFence >= 0) close(acquireFence);
    return AIMAPPER_ERROR_BAD_VALUE;
  }
  waitFence(acquireFence);
  // Memory is CPU-coherent shmem on x86; no DMA_BUF_IOCTL_SYNC needed.
  *outData = b->base;
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error unlock(buffer_handle_t buffer, int* releaseFence) {
  if (!registry().find(buffer)) return AIMAPPER_ERROR_BAD_BUFFER;
  *releaseFence = -1;
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error flushLockedBuffer(buffer_handle_t buffer) {
  return registry().find(buffer) ? AIMAPPER_ERROR_NONE
                                 : AIMAPPER_ERROR_BAD_BUFFER;
}

AIMapper_Error rereadLockedBuffer(buffer_handle_t buffer) {
  return registry().find(buffer) ? AIMAPPER_ERROR_NONE
                                 : AIMAPPER_ERROR_BAD_BUFFER;
}

std::vector<AidlPlaneLayout> planeLayouts(const BufferHandle& bh) {
  const FormatInfo* fi = formatInfo(bh.resolvedFormat);
  std::vector<AidlPlaneLayout> out;
  if (!fi) return out;
  for (uint32_t i = 0; i < bh.numPlanes; i++) {
    const PlaneFormat& pf = fi->planes[i];
    AidlPlaneLayout pl;
    for (uint32_t c = 0; c < pf.numComponents; c++) {
      PlaneLayoutComponent comp;
      comp.type = {
        "android.hardware.graphics.common.PlaneLayoutComponentType",
        pf.comps[c].type
      };
      comp.offsetInBits = pf.comps[c].offsetBits;
      comp.sizeInBits = pf.comps[c].sizeBits;
      pl.components.push_back(std::move(comp));
    }
    pl.offsetInBytes = int64_t(bh.planes[i].offset.get());
    pl.sampleIncrementInBits = pf.sampleIncrementBits;
    pl.strideInBytes = bh.planes[i].stride;
    pl.widthInSamples = (bh.width + pf.hSub - 1) / pf.hSub;
    pl.heightInSamples = (bh.height + pf.vSub - 1) / pf.vSub;
    pl.totalSizeInBytes = int64_t(bh.planes[i].size.get());
    pl.horizontalSubsampling = pf.hSub;
    pl.verticalSubsampling = pf.vSub;
    out.push_back(std::move(pl));
  }
  return out;
}

std::optional<std::vector<uint8_t>> blob(const SharedMetadata& md, int i) {
  if (md.blobLen[i] == 0) return std::nullopt;
  return std::vector<uint8_t>(md.blob[i], md.blob[i] + md.blobLen[i]);
}

int32_t getStandard(
  const Imported& b, int64_t type, void* dest, size_t destSize
) {
  const BufferHandle& bh = *b.bh;
  const SharedMetadata& md = *b.md();
  return provideStandardMetadata(
    static_cast<StandardMetadataType>(type), dest, destSize,
    [&]<StandardMetadataType T>(auto&& provide) -> int32_t {
      using S = StandardMetadataType;
      if constexpr (T == S::BUFFER_ID) {
        return provide(bh.bufferId.get());
      } else if constexpr (T == S::NAME) {
        return provide(
          std::string(
            md.name, std::min<uint32_t>(md.nameLen, SharedMetadata::kMaxName)
          )
        );
      } else if constexpr (T == S::WIDTH) {
        return provide(bh.width);
      } else if constexpr (T == S::HEIGHT) {
        return provide(bh.height);
      } else if constexpr (T == S::LAYER_COUNT) {
        return provide(bh.layerCount);
      } else if constexpr (T == S::PIXEL_FORMAT_REQUESTED) {
        return provide(static_cast<PixelFormat>(bh.format));
      } else if constexpr (T == S::PIXEL_FORMAT_FOURCC) {
        return provide(bh.fourcc);
      } else if constexpr (T == S::PIXEL_FORMAT_MODIFIER) {
        return provide(uint64_t(0));  // DRM_FORMAT_MOD_LINEAR
      } else if constexpr (T == S::USAGE) {
        return provide(static_cast<BufferUsage>(bh.usage.get()));
      } else if constexpr (T == S::ALLOCATION_SIZE) {
        return provide(bh.totalSize.get());
      } else if constexpr (T == S::PROTECTED_CONTENT) {
        return provide(uint64_t(0));
      } else if constexpr (T == S::COMPRESSION) {
        return provide(
          ExtendableType{
            "android.hardware.graphics.common.Compression",
            int64_t(Compression::NONE)
          }
        );
      } else if constexpr (T == S::INTERLACED) {
        return provide(
          ExtendableType{
            "android.hardware.graphics.common.Interlaced",
            int64_t(Interlaced::NONE)
          }
        );
      } else if constexpr (T == S::CHROMA_SITING) {
        const FormatInfo* fi = formatInfo(bh.resolvedFormat);
        bool sub = fi && fi->yuv && fi->numPlanes > 1;
        return provide(
          ExtendableType{
            "android.hardware.graphics.common.ChromaSiting",
            int64_t(sub ? ChromaSiting::SITED_INTERSTITIAL : ChromaSiting::NONE)
          }
        );
      } else if constexpr (T == S::PLANE_LAYOUTS) {
        return provide(planeLayouts(bh));
      } else if constexpr (T == S::CROP) {
        std::vector<Rect> crops(
          bh.numPlanes, Rect{0, 0, int32_t(bh.width), int32_t(bh.height)}
        );
        return provide(crops);
      } else if constexpr (T == S::DATASPACE) {
        return provide(static_cast<Dataspace>(md.dataspace));
      } else if constexpr (T == S::BLEND_MODE) {
        return provide(static_cast<BlendMode>(md.blendMode));
      } else if constexpr (T == S::SMPTE2086) {
        std::optional<Smpte2086> v;
        if (md.hasSmpte2086) {
          const float* f = md.smpte2086;
          v = Smpte2086{{f[0], f[1]}, {f[2], f[3]}, {f[4], f[5]},
                        {f[6], f[7]}, f[8],         f[9]};
        }
        return provide(v);
      } else if constexpr (T == S::CTA861_3) {
        std::optional<Cta861_3> v;
        if (md.hasCta861_3) v = Cta861_3{md.cta861_3[0], md.cta861_3[1]};
        return provide(v);
      } else if constexpr (T == S::SMPTE2094_10) {
        return provide(blob(md, 0));
      } else if constexpr (T == S::SMPTE2094_40) {
        return provide(blob(md, 1));
      } else if constexpr (T == S::SMPTE2094_50) {
        return provide(blob(md, 2));
      } else if constexpr (T == S::STRIDE) {
        return provide(bh.stridePixels);
      } else {
        return -AIMAPPER_ERROR_UNSUPPORTED;
      }
    }
  );
}

AIMapper_Error setBlob(
  SharedMetadata& md, int i, const std::optional<std::vector<uint8_t>>& v
) {
  if (!v) {
    md.blobLen[i] = 0;
    return AIMAPPER_ERROR_NONE;
  }
  if (v->size() > SharedMetadata::kMaxBlob) return AIMAPPER_ERROR_NO_RESOURCES;
  memcpy(md.blob[i], v->data(), v->size());
  md.blobLen[i] = uint32_t(v->size());
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error setStandard(
  Imported& b, int64_t type, const void* metadata, size_t size
) {
  SharedMetadata& md = *b.md();
  return applyStandardMetadata(
    static_cast<StandardMetadataType>(type), metadata, size,
    [&]<StandardMetadataType T>(auto&& value) -> AIMapper_Error {
      using S = StandardMetadataType;
      if constexpr (T == S::DATASPACE) {
        md.dataspace = static_cast<int32_t>(value);
        return AIMAPPER_ERROR_NONE;
      } else if constexpr (T == S::BLEND_MODE) {
        md.blendMode = static_cast<int32_t>(value);
        return AIMAPPER_ERROR_NONE;
      } else if constexpr (T == S::SMPTE2086) {
        if (value) {
          const Smpte2086& v = *value;
          float f[10] = {v.primaryRed.x,   v.primaryRed.y,  v.primaryGreen.x,
                         v.primaryGreen.y, v.primaryBlue.x, v.primaryBlue.y,
                         v.whitePoint.x,   v.whitePoint.y,  v.maxLuminance,
                         v.minLuminance};
          memcpy(md.smpte2086, f, sizeof(f));
        }
        md.hasSmpte2086 = value.has_value();
        return AIMAPPER_ERROR_NONE;
      } else if constexpr (T == S::CTA861_3) {
        if (value) {
          md.cta861_3[0] = value->maxContentLightLevel;
          md.cta861_3[1] = value->maxFrameAverageLightLevel;
        }
        md.hasCta861_3 = value.has_value();
        return AIMAPPER_ERROR_NONE;
      } else if constexpr (T == S::SMPTE2094_10) {
        return setBlob(md, 0, value);
      } else if constexpr (T == S::SMPTE2094_40) {
        return setBlob(md, 1, value);
      } else if constexpr (T == S::SMPTE2094_50) {
        return setBlob(md, 2, value);
      } else if constexpr (
        T == S::BUFFER_ID || T == S::NAME || T == S::WIDTH || T == S::HEIGHT ||
        T == S::LAYER_COUNT || T == S::PIXEL_FORMAT_REQUESTED || T == S::USAGE
      ) {
        return AIMAPPER_ERROR_BAD_VALUE;
      } else {
        return AIMAPPER_ERROR_UNSUPPORTED;
      }
    }
  );
}

int32_t getStandardMetadata(
  buffer_handle_t buffer, int64_t type, void* dest, size_t destSize
) {
  Imported* b = registry().find(buffer);
  if (!b) return -AIMAPPER_ERROR_BAD_BUFFER;
  return getStandard(*b, type, dest, destSize);
}

int32_t getMetadata(
  buffer_handle_t buffer, AIMapper_MetadataType type, void* dest,
  size_t destSize
) {
  Imported* b = registry().find(buffer);
  if (!b) return -AIMAPPER_ERROR_BAD_BUFFER;
  if (!type.name || strcmp(type.name, kStandardName) != 0)
    return -AIMAPPER_ERROR_UNSUPPORTED;
  return getStandard(*b, type.value, dest, destSize);
}

AIMapper_Error setStandardMetadata(
  buffer_handle_t buffer, int64_t type, const void* metadata, size_t size
) {
  Imported* b = registry().find(buffer);
  if (!b) return AIMAPPER_ERROR_BAD_BUFFER;
  return setStandard(*b, type, metadata, size);
}

AIMapper_Error setMetadata(
  buffer_handle_t buffer, AIMapper_MetadataType type, const void* metadata,
  size_t size
) {
  Imported* b = registry().find(buffer);
  if (!b) return AIMAPPER_ERROR_BAD_BUFFER;
  if (!type.name || strcmp(type.name, kStandardName) != 0)
    return AIMAPPER_ERROR_UNSUPPORTED;
  return setStandard(*b, type.value, metadata, size);
}

constexpr AIMapper_MetadataTypeDescription desc(
  StandardMetadataType t, bool settable
) {
  return {{kStandardName, int64_t(t)}, nullptr, true, settable, {}};
}

using S = StandardMetadataType;
constexpr AIMapper_MetadataTypeDescription kDescriptions[] = {
  desc(S::BUFFER_ID, false),
  desc(S::NAME, false),
  desc(S::WIDTH, false),
  desc(S::HEIGHT, false),
  desc(S::LAYER_COUNT, false),
  desc(S::PIXEL_FORMAT_REQUESTED, false),
  desc(S::PIXEL_FORMAT_FOURCC, false),
  desc(S::PIXEL_FORMAT_MODIFIER, false),
  desc(S::USAGE, false),
  desc(S::ALLOCATION_SIZE, false),
  desc(S::PROTECTED_CONTENT, false),
  desc(S::COMPRESSION, false),
  desc(S::INTERLACED, false),
  desc(S::CHROMA_SITING, false),
  desc(S::PLANE_LAYOUTS, false),
  desc(S::CROP, false),
  desc(S::DATASPACE, true),
  desc(S::BLEND_MODE, true),
  desc(S::SMPTE2086, true),
  desc(S::CTA861_3, true),
  desc(S::SMPTE2094_10, true),
  desc(S::SMPTE2094_40, true),
  desc(S::SMPTE2094_50, true),
  desc(S::STRIDE, false),
};

AIMapper_Error listSupportedMetadataTypes(
  const AIMapper_MetadataTypeDescription** outList, size_t* outCount
) {
  *outList = kDescriptions;
  *outCount = std::size(kDescriptions);
  return AIMAPPER_ERROR_NONE;
}

void dumpOne(const Imported& b, AIMapper_DumpBufferCallback cb, void* ctx) {
  std::vector<uint8_t> buf(4096);
  for (const auto& d : kDescriptions) {
    int32_t n = getStandard(b, d.metadataType.value, buf.data(), buf.size());
    if (n > int32_t(buf.size())) {
      buf.resize(n);
      n = getStandard(b, d.metadataType.value, buf.data(), buf.size());
    }
    if (n > 0) cb(ctx, d.metadataType, buf.data(), size_t(n));
  }
}

AIMapper_Error dumpBuffer(
  buffer_handle_t buffer, AIMapper_DumpBufferCallback cb, void* ctx
) {
  Imported* b = registry().find(buffer);
  if (!b) return AIMAPPER_ERROR_BAD_BUFFER;
  dumpOne(*b, cb, ctx);
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error dumpAllBuffers(
  AIMapper_BeginDumpBufferCallback begin, AIMapper_DumpBufferCallback cb,
  void* ctx
) {
  registry().forEach([&](const Imported& b) {
    begin(ctx);
    dumpOne(b, cb, ctx);
  });
  return AIMAPPER_ERROR_NONE;
}

AIMapper_Error getReservedRegion(
  buffer_handle_t buffer, void** outRegion, uint64_t* outSize
) {
  Imported* b = registry().find(buffer);
  if (!b) return AIMAPPER_ERROR_BAD_BUFFER;
  const uint64_t size = b->bh->reservedSize.get();
  *outRegion = size ? b->base + b->bh->reservedOffset.get() : nullptr;
  *outSize = size;
  return AIMAPPER_ERROR_NONE;
}

AIMapper gMapper = {
  .version = AIMAPPER_VERSION_5,
  .v5 =
    {
      .importBuffer = importBuffer,
      .freeBuffer = freeBuffer,
      .getTransportSize = getTransportSize,
      .lock = lock,
      .unlock = unlock,
      .flushLockedBuffer = flushLockedBuffer,
      .rereadLockedBuffer = rereadLockedBuffer,
      .getMetadata = getMetadata,
      .getStandardMetadata = getStandardMetadata,
      .setMetadata = setMetadata,
      .setStandardMetadata = setStandardMetadata,
      .listSupportedMetadataTypes = listSupportedMetadataTypes,
      .dumpBuffer = dumpBuffer,
      .dumpAllBuffers = dumpAllBuffers,
      .getReservedRegion = getReservedRegion,
    },
  .v6 = {},
};

}  // namespace
}  // namespace linuxapk

extern "C" {
__attribute__((visibility("default"))) uint32_t ANDROID_HAL_STABLEC_VERSION =
  AIMAPPER_VERSION_5;

__attribute__((visibility("default"))) AIMapper_Error
AIMapper_loadIMapper(AIMapper* _Nullable* _Nonnull outImplementation) {
  *outImplementation = &linuxapk::gMapper;
  return AIMAPPER_ERROR_NONE;
}
}
