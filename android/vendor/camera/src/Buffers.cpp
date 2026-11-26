// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-camera"

#include <cstring>

#include <sys/mman.h>
#include <unistd.h>

#include "Buffers.h"

#include "innkeeper/log.h"

namespace innkeeper::camera {

namespace {

// android.hardware.camera.device.CameraBlob, at the end of a BLOB buffer.
struct CameraBlob {
  int32_t blobId;  // CameraBlobId::JPEG
  int32_t blobSizeBytes;
};
constexpr int32_t kBlobJpeg = 0x00ff;

}  // namespace

std::shared_ptr<Buffer> Buffer::import(
  const std::vector<int>& fds, const std::vector<int32_t>& ints
) {
  if (
    fds.size() != size_t(kHandleNumFds) || ints.size() != size_t(kHandleNumInts)
  ) {
    return nullptr;
  }
  auto b = std::make_shared<Buffer>();
  memcpy(&b->mHandle.magic, ints.data(), ints.size() * sizeof(int32_t));
  const BufferHandle& h = b->mHandle;
  if (
    h.magic != kHandleMagic || h.version != kHandleVersion ||
    h.numPlanes > kMaxPlanes
  ) {
    return nullptr;
  }
  uint64_t size = h.totalSize.get();
  for (uint32_t i = 0; i < h.numPlanes; i++) {
    if (h.planes[i].offset.get() + h.planes[i].size.get() > size)
      return nullptr;
  }
  void* p = mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, fds[0], 0);
  if (p == MAP_FAILED) {
    LOGE(
      "mmap of a %llu-byte buffer: %s", (unsigned long long)size,
      strerror(errno)
    );
    return nullptr;
  }
  b->mBase = static_cast<uint8_t*>(p);
  b->mSize = size;
  return b;
}

Buffer::~Buffer() {
  if (mBase) munmap(mBase, mSize);
}

bool Buffer::yuv(YuvTarget* t) const {
  const BufferHandle& h = mHandle;
  auto plane = [&](uint32_t i) { return mBase + h.planes[i].offset.get(); };
  t->width = h.width;
  t->height = h.height;
  t->y = plane(0);
  t->yStride = h.planes[0].stride;
  switch (h.resolvedFormat) {
    case fmt::YCBCR_420_888:  // NV12
    case fmt::YCRCB_420_SP:   // NV21
      if (h.numPlanes != 2) return false;
      t->cStride = h.planes[1].stride;
      t->chromaStep = 2;
      t->cb = plane(1) + (h.resolvedFormat == fmt::YCRCB_420_SP);
      t->cr = plane(1) + (h.resolvedFormat == fmt::YCBCR_420_888);
      return true;
    case fmt::YV12:  // Y, Cr, Cb
      if (h.numPlanes != 3) return false;
      t->cStride = h.planes[1].stride;
      t->chromaStep = 1;
      t->cr = plane(1);
      t->cb = plane(2);
      return true;
    default:
      return false;
  }
}

bool Buffer::writeJpeg(const std::vector<uint8_t>& jpeg) const {
  if (mHandle.resolvedFormat != fmt::BLOB) return false;
  // A BLOB buffer is width bytes long.
  size_t size = std::min<size_t>(mHandle.width, mSize);
  if (jpeg.size() + sizeof(CameraBlob) > size) return false;
  memcpy(mBase, jpeg.data(), jpeg.size());
  CameraBlob blob{kBlobJpeg, int32_t(jpeg.size())};
  memcpy(mBase + size - sizeof(blob), &blob, sizeof(blob));
  return true;
}

}  // namespace innkeeper::camera
