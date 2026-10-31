// SPDX-License-Identifier: AGPL-3.0-or-later
// libdisplay_buffer_vdi_impl.z.so: OpenHarmony's buffer VDI, which the
// image's allocator_host calls to allocate and every process that maps a
// buffer loads (passthrough). Buffers are sealed memfds turned into dma-bufs
// by /dev/udmabuf, like Android's CPU buffers (android/vendor/graphics):
// LINEAR, CPU-mappable, and importable by llvmpipe and by the bridge's
// compositor. Without udmabuf they stay plain memfds, which CPU users can still
// map.
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "hdi.h"
#include "log.h"

#define LOG_TAG "innkeeper-buffer"

namespace {

// linux/udmabuf.h
struct udmabuf_create {
  uint32_t memfd;
  uint32_t flags;
  uint64_t offset;
  uint64_t size;
};
constexpr uint32_t kUdmabufCloexec = 1;
constexpr unsigned long kUdmabufCreate = _IOW('u', 0x42, udmabuf_create);

// Row pitch in bytes for a LINEAR buffer; 256-byte aligned as Android's
// allocator does, which RADV and the compositors accept.
constexpr uint32_t kPitchAlign = 256;

uint64_t alignUp(uint64_t v, uint64_t a) { return (v + a - 1) / a * a; }

// Bytes per pixel of the first plane, and the total height in rows of that
// pitch (YUV 4:2:0 puts both chroma planes below the luma at half size).
bool layout(uint32_t format, uint32_t height, uint32_t* bpp, uint64_t* rows) {
  *rows = height;
  switch (format) {
    case PIXEL_FMT_RGBX_8888:
    case PIXEL_FMT_RGBA_8888:
    case PIXEL_FMT_BGRX_8888:
    case PIXEL_FMT_BGRA_8888:
    case PIXEL_FMT_RGBA_1010102:
      *bpp = 4;
      return true;
    case PIXEL_FMT_RGB_888:
      *bpp = 3;
      return true;
    case PIXEL_FMT_RGB_565:
      *bpp = 2;
      return true;
    case PIXEL_FMT_YCBCR_420_SP:
    case PIXEL_FMT_YCRCB_420_SP:
    case PIXEL_FMT_YCBCR_420_P:
    case PIXEL_FMT_YCRCB_420_P:
      *bpp = 1;
      *rows = alignUp(height, 2) * 3 / 2;
      return true;
    default:
      return false;
  }
}

int udmabufDevice() {
  static int dev = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
  return dev;
}

// The interface's vtable order (drivers_peripheral
// display/buffer/hdi_service/include/idisplay_buffer_vdi.h, OH 7.0). The
// references are pointers in the ABI; the vector arguments are opaque.
struct Opaque;
class IDisplayBufferVdi {
 public:
  virtual ~IDisplayBufferVdi() = default;
  virtual int32_t AllocMem(
    const AllocInfo& info, BufferHandle*& handle
  ) const = 0;
  virtual void FreeMem(const BufferHandle& handle) const = 0;
  virtual void* Mmap(const BufferHandle& handle) const = 0;
  virtual int32_t Unmap(const BufferHandle& handle) const = 0;
  virtual int32_t FlushCache(const BufferHandle& handle) const = 0;
  virtual int32_t InvalidateCache(const BufferHandle& handle) const = 0;
  virtual int32_t IsSupportedAlloc(
    const Opaque& infos, Opaque& supported
  ) const = 0;
  virtual int32_t RegisterBuffer(const BufferHandle& handle) = 0;
  virtual int32_t SetMetadata(
    const BufferHandle& handle, uint32_t key, const Opaque& value
  ) = 0;
  virtual int32_t GetMetadata(
    const BufferHandle& handle, uint32_t key, Opaque& value
  ) = 0;
  virtual int32_t ListMetadataKeys(
    const BufferHandle& handle, Opaque& keys
  ) = 0;
  virtual int32_t EraseMetadataKey(
    const BufferHandle& handle, uint32_t key
  ) = 0;
  virtual int32_t GetImageLayout(
    const BufferHandle& handle, Opaque& layout
  ) const = 0;
  virtual int32_t IsSupportAllocPassthrough(const AllocInfo& info) const = 0;
  virtual int32_t ReAllocMem(
    const AllocInfo& info, const BufferHandle& in, BufferHandle*& out
  ) const = 0;
  virtual int32_t CloneDmaBufferHandle(
    const BufferHandle& in, BufferHandle*& out
  ) const = 0;
};

class BufferVdi final : public IDisplayBufferVdi {
 public:
  int32_t AllocMem(
    const AllocInfo& info, BufferHandle*& handle
  ) const override {
    uint32_t bpp = 0;
    uint64_t rows = 0;
    if (
      info.width == 0 || info.height == 0 || info.width > 16384 ||
      info.height > 16384 || !layout(info.format, info.height, &bpp, &rows)
    ) {
      LOGE("unsupported %ux%u format %u", info.width, info.height, info.format);
      return HDF_ERR_NOT_SUPPORT;
    }
    const uint64_t stride = alignUp(uint64_t(info.width) * bpp, kPitchAlign);
    uint64_t size = stride * rows;
    if ((info.usage & HBM_USE_ASSIGN_SIZE) && info.expectedSize > size) {
      size = info.expectedSize;
    }
    size = alignUp(size, 4096);
    if (size > INT32_MAX) return HDF_ERR_NOT_SUPPORT;

    int fd = memfd_create("ohos-buffer", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (fd < 0 || ftruncate(fd, off_t(size)) != 0) {
      LOGE("memfd (%llu bytes): %s", (unsigned long long)size, strerror(errno));
      if (fd >= 0) close(fd);
      return HDF_FAILURE;
    }
    if (const int dev = udmabufDevice(); dev >= 0) {
      fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK);
      udmabuf_create create{uint32_t(fd), kUdmabufCloexec, 0, size};
      if (const int buf = ioctl(dev, kUdmabufCreate, &create); buf >= 0) {
        close(fd);
        fd = buf;
      } else {
        LOGW("UDMABUF_CREATE: %s; a CPU-only memfd", strerror(errno));
      }
    }
    auto* h = static_cast<BufferHandle*>(calloc(1, sizeof(BufferHandle)));
    if (!h) {
      close(fd);
      return HDF_FAILURE;
    }
    h->fd = fd;
    h->width = int32_t(info.width);
    h->stride = int32_t(stride);
    h->height = int32_t(info.height);
    h->size = int32_t(size);
    h->format = int32_t(info.format);
    h->usage = info.usage;
    handle = h;
    return HDF_SUCCESS;
  }

  // Handles come from AllocMem or from the IPC unmarshaller, both malloc'd.
  void FreeMem(const BufferHandle& handle) const override {
    auto* h = const_cast<BufferHandle*>(&handle);
    if (h->virAddr) munmap(h->virAddr, size_t(h->size));
    if (h->fd >= 0) close(h->fd);
    for (uint32_t i = 0; i < h->reserveFds; i++) {
      if (h->reserve[i] >= 0) close(h->reserve[i]);
    }
    free(h);
  }

  void* Mmap(const BufferHandle& handle) const override {
    auto* h = const_cast<BufferHandle*>(&handle);
    if (h->virAddr) return h->virAddr;
    void* p = mmap(
      nullptr, size_t(h->size), PROT_READ | PROT_WRITE, MAP_SHARED, h->fd, 0
    );
    if (p == MAP_FAILED) {
      LOGE("mmap fd %d (%d bytes): %s", h->fd, h->size, strerror(errno));
      return nullptr;
    }
    h->virAddr = p;
    return p;
  }

  int32_t Unmap(const BufferHandle& handle) const override {
    auto* h = const_cast<BufferHandle*>(&handle);
    if (!h->virAddr) return HDF_FAILURE;
    munmap(h->virAddr, size_t(h->size));
    h->virAddr = nullptr;
    return HDF_SUCCESS;
  }

  // CPU and GPU caches are coherent for system memory on x86.
  int32_t FlushCache(const BufferHandle&) const override { return HDF_SUCCESS; }
  int32_t InvalidateCache(const BufferHandle&) const override {
    return HDF_SUCCESS;
  }
  int32_t RegisterBuffer(const BufferHandle&) override { return HDF_SUCCESS; }

  int32_t IsSupportedAlloc(const Opaque&, Opaque&) const override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t SetMetadata(const BufferHandle&, uint32_t, const Opaque&) override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t GetMetadata(const BufferHandle&, uint32_t, Opaque&) override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t ListMetadataKeys(const BufferHandle&, Opaque&) override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t EraseMetadataKey(const BufferHandle&, uint32_t) override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t GetImageLayout(const BufferHandle&, Opaque&) const override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t IsSupportAllocPassthrough(const AllocInfo&) const override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t ReAllocMem(
    const AllocInfo&, const BufferHandle&, BufferHandle*&
  ) const override {
    return HDF_ERR_NOT_SUPPORT;
  }
  int32_t CloneDmaBufferHandle(
    const BufferHandle&, BufferHandle*&
  ) const override {
    return HDF_ERR_NOT_SUPPORT;
  }
};

}  // namespace

extern "C" __attribute__((visibility("default"))) IDisplayBufferVdi*
CreateDisplayBufferVdi() {
  return new BufferVdi();
}

extern "C" __attribute__((visibility("default"))) void DestroyDisplayBufferVdi(
  IDisplayBufferVdi* vdi
) {
  delete vdi;
}
