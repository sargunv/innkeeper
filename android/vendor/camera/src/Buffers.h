// SPDX-License-Identifier: AGPL-3.0-or-later
// Gralloc buffers the camera HAL fills. Our allocator's native handle
// (graphics/common/include/innkeeper/gralloc_handle.h) is the format, so the
// HAL maps the pixel fd itself instead of going through the mapper. Camera
// buffers carry CPU write usage, so they are linear memfd or udmabuf memory.
#pragma once

#include <cstdint>
#include <memory>
#include <vector>

#include "Convert.h"
#include "innkeeper/gralloc_handle.h"

namespace innkeeper::camera {

class Buffer {
 public:
  // Takes the handle's ints and dups its pixel fd. Returns null for a handle
  // that isn't ours or can't be mapped.
  static std::shared_ptr<Buffer> import(
    const std::vector<int>& fds, const std::vector<int32_t>& ints
  );
  ~Buffer();

  int32_t format() const { return mHandle.resolvedFormat; }

  // The buffer as a YCbCr 4:2:0 target, if its format is one.
  bool yuv(YuvTarget* t) const;

  // Writes a JPEG and the CameraBlob trailer of a BLOB buffer.
  bool writeJpeg(const std::vector<uint8_t>& jpeg) const;

 private:
  BufferHandle mHandle{};
  uint8_t* mBase = nullptr;
  size_t mSize = 0;
};

}  // namespace innkeeper::camera
