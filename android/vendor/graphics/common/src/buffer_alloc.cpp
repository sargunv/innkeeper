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

#include "innkeeper/buffer_alloc.h"

#include "innkeeper/gralloc_handle.h"
#include "innkeeper/log.h"
#include "innkeeper/native_handle.h"

namespace innkeeper {
namespace {

std::atomic<bool> gUdmabufOk{false};

int udmabufDevice() {
  static int fd = [] {
    const char* path = getenv("INNKEEPER_UDMABUF");
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
  const AllocRequest& req, native_handle_t** outHandle, GpuAllocate gpuAllocate
) {
  Layout l;
  AllocStatus st = checkRequest(req, &l);
  if (st != AllocStatus::Ok) return st;

  // CPU mappings (cursors and our software codecs and cameras read them too)
  // and multi-plane formats retain the existing LINEAR path, as in minigbm.
  const bool gpuOnly =
    !(req.usage &
      (usage::CPU_READ_MASK | usage::CPU_WRITE_MASK | usage::COMPOSER_CURSOR |
       usage::PROTECTED | usage::VIDEO_ENCODER | usage::VIDEO_DECODER |
       usage::CAMERA_OUTPUT | usage::CAMERA_INPUT)) &&
    (req.usage & (usage::GPU_RENDER_TARGET | usage::COMPOSER_OVERLAY |
                  usage::COMPOSER_CLIENT_TARGET)) &&
    l.numPlanes == 1 && req.layerCount == 1;
  const Layout linear = l;
  GpuBuffer gpu = gpuOnly && gpuAllocate ? gpuAllocate(req, &l) : GpuBuffer{};
  // Importers need the driver's plane inside the exported allocation.
  if (
    gpu.fd >= 0 && (l.planes[0].offset + l.planes[0].size > l.layerSize ||
                    lseek(gpu.fd, 0, SEEK_END) < off_t(l.pixelBytes))
  ) {
    LOGW("GPU layout outside its allocation; using LINEAR");
    close(gpu.fd);
    gpu = {};
    l = linear;
  }
  const uint64_t total = l.pixelBytes;
  const uint64_t reservedSize = alignUp(req.reservedSize, kPageSize);

  SharedMetadata md{};
  md.magic = SharedMetadata::kMagic;
  md.modifier = gpu.modifier;
  md.dataspace = 0;  // UNKNOWN
  md.blendMode = 0;  // INVALID until set
  md.nameLen =
    uint32_t(std::min<size_t>(req.name.size(), SharedMetadata::kMaxName));
  memcpy(md.name, req.name.data(), md.nameLen);
  const std::string name(req.name.substr(0, 200));
  int mdFd = memfd_create(
    ("gralloc-metadata:" + name).c_str(), MFD_CLOEXEC | MFD_ALLOW_SEALING
  );
  if (
    mdFd < 0 || ftruncate(mdFd, off_t(kReservedOffset + reservedSize)) != 0 ||
    pwrite(mdFd, &md, sizeof(md), 0) != ssize_t(sizeof(md))
  ) {
    LOGE("buffer metadata: %s", strerror(errno));
    if (mdFd >= 0) close(mdFd);
    if (gpu.fd >= 0) close(gpu.fd);
    return AllocStatus::NoResources;
  }
  fcntl(mdFd, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL);

  int bufFd = gpu.fd;
  uint32_t flags = gpu.fd >= 0 ? kFlagDmabuf | kFlagTiled : 0;
  if (gpu.fd < 0) {
    int memfd = memfd_create(
      ("gralloc:" + name).c_str(), MFD_CLOEXEC | MFD_ALLOW_SEALING
    );
    if (memfd < 0 || ftruncate(memfd, off_t(total)) != 0) {
      LOGE(
        "pixels (%llu bytes): %s", (unsigned long long)total, strerror(errno)
      );
      if (memfd >= 0) close(memfd);
      close(mdFd);
      return AllocStatus::NoResources;
    }
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
        close(mdFd);
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
  }

  native_handle_t* nh = nativeHandleCreate(kHandleNumFds, kHandleNumInts);
  if (!nh) {
    close(bufFd);
    close(mdFd);
    return AllocStatus::NoResources;
  }
  auto* bh = reinterpret_cast<BufferHandle*>(nh);
  bh->fd = bufFd;
  bh->metadataFd = mdFd;
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
  bh->reservedSize.set(req.reservedSize);
  bh->numPlanes = l.numPlanes;
  for (uint32_t i = 0; i < l.numPlanes; i++) {
    bh->planes[i].offset.set(l.planes[i].offset);
    bh->planes[i].stride = l.planes[i].stride;
    bh->planes[i].size.set(l.planes[i].size);
  }
  *outHandle = nh;
  return AllocStatus::Ok;
}

}  // namespace innkeeper
