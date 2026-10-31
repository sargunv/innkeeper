// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-hwc"
#include <string.h>
#include <sys/mman.h>

#include "Buffer.h"

#include "Fence.h"
#include "innkeeper/log.h"
#include "innkeeper/native_handle.h"

namespace innkeeper::hwc {

BufferRef::~BufferRef() {
  if (mMap) munmap(mMap, mMapSize);
  if (mMetadata) munmap(mMetadata, kPageSize);
  nativeHandleCloseAndDelete(mHandle);
}

bool BufferRef::setPresentationToken(uint64_t token) {
  const auto* h = info();
  if (!h) return false;
  if (!mMetadata) {
    void* p = mmap(
      nullptr, kPageSize, PROT_READ | PROT_WRITE, MAP_SHARED, h->metadataFd, 0
    );
    if (p == MAP_FAILED) return false;
    mMetadata = p;
  }
  auto* md = static_cast<SharedMetadata*>(mMetadata);
  __atomic_store_n(&md->presentationToken, token, __ATOMIC_RELEASE);
  return true;
}

const uint8_t* BufferRef::map() {
  if (mMap) return static_cast<const uint8_t*>(mMap);
  const BufferHandle* bh = info();
  if (!bh) return nullptr;
  void* p =
    mmap(nullptr, bh->totalSize.get(), PROT_READ, MAP_SHARED, bh->fd, 0);
  if (p == MAP_FAILED) {
    LOGW(
      "mmap of buffer %llu failed: %s", (unsigned long long)bh->bufferId.get(),
      strerror(errno)
    );
    return nullptr;
  }
  mMap = p;
  mMapSize = bh->totalSize.get();
  return static_cast<const uint8_t*>(mMap);
}

Error BufferSlots::update(const c3::Buffer& buffer, BufferPtr* out) {
  if (buffer.slot < 0 || buffer.slot > 4096) return Error::BadParameter;
  const size_t slot = size_t(buffer.slot);
  if (slot >= mSlots.size()) mSlots.resize(slot + 1);
  if (buffer.handle) {
    const auto& h = *buffer.handle;
    native_handle_t* nh =
      nativeHandleCreate(int(h.fds.size()), int(h.ints.size()));
    if (!nh) return Error::NoResources;
    for (size_t i = 0; i < h.fds.size(); i++) {
      nh->data[i] = dupFd(h.fds[i].get());
      if (nh->data[i] < 0) {
        nh->numFds = int(i);
        nativeHandleCloseAndDelete(nh);
        return Error::NoResources;
      }
    }
    if (!h.ints.empty()) {
      memcpy(
        &nh->data[h.fds.size()], h.ints.data(), h.ints.size() * sizeof(int)
      );
    }
    mSlots[slot] = std::make_shared<BufferRef>(nh);
  }
  if (!mSlots[slot]) return Error::BadParameter;
  *out = mSlots[slot];
  return Error::None;
}

}  // namespace innkeeper::hwc
