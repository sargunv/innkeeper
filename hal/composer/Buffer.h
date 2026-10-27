// SPDX-License-Identifier: AGPL-3.0-or-later
// Buffers received from SurfaceFlinger, cached per slot as HWC3 requires.
#pragma once

#include <aidl/android/hardware/graphics/composer3/Buffer.h>
#include <cutils/native_handle.h>

#include <memory>
#include <vector>

#include "Common.h"
#include "linuxapk/gralloc_handle.h"

namespace linuxapk::hwc {

class BufferRef {
  public:
    explicit BufferRef(native_handle_t* owned) : mHandle(owned) {}
    ~BufferRef();
    BufferRef(const BufferRef&) = delete;
    BufferRef& operator=(const BufferRef&) = delete;

    const native_handle_t* handle() const { return mHandle; }
    // Our gralloc's handle info, or nullptr for foreign buffers.
    const BufferHandle* info() const { return asBufferHandle(mHandle); }
    // CPU mapping of the whole fd (lazy, cached); nullptr on failure.
    const uint8_t* map();

  private:
    native_handle_t* mHandle;
    void* mMap = nullptr;
    size_t mMapSize = 0;
};
using BufferPtr = std::shared_ptr<BufferRef>;

class BufferSlots {
  public:
    void resize(size_t n) { mSlots.resize(n); }
    // Applies a composer3 Buffer: a non-null handle replaces the slot (dup'ing
    // its fds), a null handle selects the cached one.
    Error update(const c3::Buffer& buffer, BufferPtr* out);
    void clear(int32_t slot);
    void clearAll() { mSlots.clear(); }

  private:
    std::vector<BufferPtr> mSlots;
};

}  // namespace linuxapk::hwc
