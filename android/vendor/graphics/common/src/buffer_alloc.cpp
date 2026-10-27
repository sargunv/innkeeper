// SPDX-License-Identifier: AGPL-3.0-or-later
#include <algorithm>
#include <atomic>
#include <mutex>
#include <string>

#include <errno.h>
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "linuxapk/buffer_alloc.h"

#include "linuxapk/gralloc_handle.h"
#include "linuxapk/log.h"
#include "linuxapk/native_handle.h"

namespace linuxapk {
namespace {

std::atomic<bool> gUdmabufOk{false};

int udmabufDevice() {
  static int fd = [] {
    const char* path = getenv("LINUXAPK_UDMABUF");
    int f = open(path ? path : "/dev/udmabuf", O_RDWR | O_CLOEXEC);
    if (f < 0)
      LOGW("udmabuf unavailable (%s); falling back to memfd", strerror(errno));
    return f;
  }();
  return fd;
}

uint64_t nextBufferId() {
  static std::atomic<uint32_t> counter{0};
  return uint64_t(uint32_t(getpid())) << 32 | ++counter;
}

}  // namespace

bool udmabufAvailable() { return gUdmabufOk.load(); }

bool openUdmabufDevice() { return udmabufDevice() >= 0; }

AllocStatus checkRequest(const AllocRequest& req, Layout* outLayout) {
  if (req.width == 0 || req.height == 0 || req.layerCount == 0) {
    return AllocStatus::BadDescriptor;
  }
  if (req.usage & usage::PROTECTED) return AllocStatus::Unsupported;
  if ((req.usage & usage::NEEDS_DMABUF) && udmabufDevice() < 0) {
    // GPU/composer consumers can't import a plain memfd.
    return AllocStatus::Unsupported;
  }
  Layout l;
  if (!computeLayout(
        req.format, req.width, req.height, req.layerCount, req.usage, &l
      )) {
    return AllocStatus::Unsupported;
  }
  if (outLayout) *outLayout = l;
  return AllocStatus::Ok;
}

AllocStatus allocateBuffer(
  const AllocRequest& req, native_handle_t** outHandle
) {
  Layout l;
  AllocStatus st = checkRequest(req, &l);
  if (st != AllocStatus::Ok) return st;

  const uint64_t metadataOffset = l.pixelBytes;
  const uint64_t reservedOffset = metadataOffset + kPageSize;
  const uint64_t reservedSize = alignUp(req.reservedSize, kPageSize);
  const uint64_t total = reservedOffset + reservedSize;

  std::string memfdName = "gralloc:" + std::string(req.name.substr(0, 200));
  int memfd = memfd_create(memfdName.c_str(), MFD_CLOEXEC | MFD_ALLOW_SEALING);
  if (memfd < 0) {
    LOGE("memfd_create: %s", strerror(errno));
    return AllocStatus::NoResources;
  }
  if (ftruncate(memfd, off_t(total)) != 0) {
    LOGE("ftruncate(%llu): %s", (unsigned long long)total, strerror(errno));
    close(memfd);
    return AllocStatus::NoResources;
  }

  SharedMetadata md{};
  md.magic = SharedMetadata::kMagic;
  md.dataspace = 0;  // UNKNOWN
  md.blendMode = 0;  // INVALID until set
  md.nameLen =
    uint32_t(std::min<size_t>(req.name.size(), SharedMetadata::kMaxName));
  memcpy(md.name, req.name.data(), md.nameLen);
  if (
    pwrite(memfd, &md, sizeof(md), off_t(metadataOffset)) != ssize_t(sizeof(md))
  ) {
    LOGE("pwrite metadata: %s", strerror(errno));
    close(memfd);
    return AllocStatus::NoResources;
  }

  int bufFd = -1;
  uint32_t flags = 0;
  int dev = udmabufDevice();
  if (dev >= 0) {
    fcntl(memfd, F_ADD_SEALS, F_SEAL_SHRINK);
    struct udmabuf_create create{};
    create.memfd = uint32_t(memfd);
    create.flags = UDMABUF_FLAGS_CLOEXEC;
    create.offset = 0;
    create.size = total;
    bufFd = ioctl(dev, UDMABUF_CREATE, &create);
    if (bufFd >= 0) {
      flags |= kFlagDmabuf;
      close(memfd);
      gUdmabufOk = true;
    } else if (req.usage & usage::NEEDS_DMABUF) {
      // e.g. larger than udmabuf.size_limit_mb (64 MiB by default)
      LOGE(
        "UDMABUF_CREATE(%llu bytes): %s; GPU/composer usage 0x%llx needs a "
        "dma-buf",
        (unsigned long long)total, strerror(errno),
        (unsigned long long)req.usage
      );
      close(memfd);
      return AllocStatus::NoResources;
    } else {
      LOGW(
        "UDMABUF_CREATE(%llu bytes): %s; CPU-only buffer, using memfd",
        (unsigned long long)total, strerror(errno)
      );
    }
  }
  if (bufFd < 0) {
    fcntl(memfd, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL);
    bufFd = memfd;
  }

  native_handle_t* nh = nativeHandleCreate(kHandleNumFds, kHandleNumInts);
  if (!nh) {
    close(bufFd);
    return AllocStatus::NoResources;
  }
  auto* bh = reinterpret_cast<BufferHandle*>(nh);
  bh->fd = bufFd;
  bh->magic = kHandleMagic;
  bh->version = kHandleVersion;
  bh->flags = flags;
  bh->width = l.width;
  bh->height = l.height;
  bh->layerCount = l.layerCount;
  bh->format = l.format;
  bh->resolvedFormat = l.resolvedFormat;
  bh->fourcc = l.fourcc;
  bh->stridePixels = l.stridePixels;
  bh->usage.set(req.usage);
  bh->bufferId.set(nextBufferId());
  bh->totalSize.set(total);
  bh->layerSize.set(l.layerSize);
  bh->metadataOffset.set(metadataOffset);
  bh->reservedOffset.set(reservedOffset);
  bh->reservedSize.set(
    req.reservedSize
  );  // as requested; region is page aligned
  bh->numPlanes = l.numPlanes;
  for (uint32_t i = 0; i < l.numPlanes; i++) {
    bh->planes[i].offset.set(l.planes[i].offset);
    bh->planes[i].stride = l.planes[i].stride;
    bh->planes[i].size.set(l.planes[i].size);
  }
  *outHandle = nh;
  return AllocStatus::Ok;
}

}  // namespace linuxapk
