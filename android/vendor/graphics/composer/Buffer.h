// SPDX-License-Identifier: AGPL-3.0-or-later
// Buffers received from SurfaceFlinger, cached per slot as HWC3 requires.
#pragma once

#include <memory>
#include <vector>

#include <aidl/android/hardware/graphics/composer3/Buffer.h>
#include <cutils/native_handle.h>

#include "Common.h"
#include "innkeeper/gralloc_handle.h"

namespace innkeeper::hwc {

class BufferRef {
 public:
  explicit BufferRef(native_handle_t* owned) : mHandle(owned) {}
  ~BufferRef();
  BufferRef(const BufferRef&) = delete;
  BufferRef& operator=(const BufferRef&) = delete;

  const native_handle_t* handle() const { return mHandle; }
  // Our gralloc's handle info, or nullptr for foreign buffers.
  const BufferHandle* info() const { return asBufferHandle(mHandle); }
  // Maps the whole fd for CPU access on first use and caches the mapping.
  // Returns nullptr on failure.
  const uint8_t* map();
  bool setPresentationToken(uint64_t token);

 private:
  native_handle_t* mHandle;
  void* mMetadata = nullptr;
  void* mMap = nullptr;
  size_t mMapSize = 0;
};
using BufferPtr = std::shared_ptr<BufferRef>;

class BufferSlots {
 public:
  void resize(size_t n) { mSlots.resize(n); }
  // Applies a composer3 Buffer. A non-null handle replaces the slot with a
  // dup of its fds, and a null handle selects the cached one.
  Error update(const c3::Buffer& buffer, BufferPtr* out);

 private:
  std::vector<BufferPtr> mSlots;
};

}  // namespace innkeeper::hwc
