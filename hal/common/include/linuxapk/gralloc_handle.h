// SPDX-License-Identifier: AGPL-3.0-or-later
// The buffer_handle_t layout shared by our allocator, mapper and composer.
//
// One fd: a dma-buf from /dev/udmabuf (or a plain memfd if udmabuf is not
// available, flagged by kFlagDmabuf being clear). The fd covers:
//   [0, pixelBytes)                      pixel data, layerCount layers
//   [metadataOffset, +kPageSize)         SharedMetadata (mutable metadata)
//   [reservedOffset, +reservedSize)      client reserved region (optional)
// Planes are linear (DRM_FORMAT_MOD_LINEAR) at the offsets/strides below, so
// GPU drivers and Wayland (linux-dmabuf) can import fd 0 directly.
#pragma once

#include <cstddef>
#include <cstdint>

#include <cutils/native_handle.h>

#include "linuxapk/layout.h"

namespace linuxapk {

constexpr uint32_t kHandleMagic = 0x4b50414c;  // "LAPK"
constexpr uint32_t kHandleVersion = 1;
constexpr uint32_t kFlagDmabuf = 1u << 0;

struct U64 {
  uint32_t lo, hi;
  uint64_t get() const { return uint64_t(hi) << 32 | lo; }
  void set(uint64_t v) {
    lo = uint32_t(v);
    hi = uint32_t(v >> 32);
  }
};

struct HandlePlane {
  U64 offset;
  uint32_t stride;  // bytes
  U64 size;
};

struct BufferHandle {
  native_handle_t base;
  int fd;
  // ---- ints ----
  uint32_t magic;
  uint32_t version;
  uint32_t flags;
  uint32_t width, height, layerCount;
  int32_t format;          // as requested (PIXEL_FORMAT_REQUESTED)
  int32_t resolvedFormat;  // concrete layout
  uint32_t fourcc;
  uint32_t stridePixels;
  U64 usage;
  U64 bufferId;
  U64 totalSize;  // size of the fd
  U64 layerSize;
  U64 metadataOffset;
  U64 reservedOffset;
  U64 reservedSize;
  uint32_t numPlanes;
  HandlePlane planes[kMaxPlanes];
};

constexpr int kHandleNumFds = 1;
constexpr int kHandleNumInts =
  int((sizeof(BufferHandle) - offsetof(BufferHandle, magic)) / sizeof(int));
static_assert(offsetof(BufferHandle, fd) == sizeof(native_handle_t));
static_assert(
  offsetof(BufferHandle, magic) == sizeof(native_handle_t) + sizeof(int)
);
static_assert(sizeof(BufferHandle) % sizeof(int) == 0);

// Validates layout/magic; returns nullptr if h is not one of ours.
inline const BufferHandle* asBufferHandle(const native_handle_t* h) {
  if (
    !h || h->version != int(sizeof(native_handle_t)) ||
    h->numFds != kHandleNumFds || h->numInts != kHandleNumInts
  ) {
    return nullptr;
  }
  auto* bh = reinterpret_cast<const BufferHandle*>(h);
  if (bh->magic != kHandleMagic || bh->version != kHandleVersion)
    return nullptr;
  return bh;
}

// Mutable per-buffer metadata, stored in the buffer's own memory so every
// process that imports the buffer sees the same values.
struct SharedMetadata {
  static constexpr uint32_t kMagic = 0x4154454d;  // "META"
  static constexpr uint32_t kMaxName = 128;
  static constexpr uint32_t kMaxBlob = 1024;
  uint32_t magic;
  int32_t dataspace;
  int32_t blendMode;
  uint32_t nameLen;
  char name[kMaxName];
  uint32_t hasSmpte2086;
  float smpte2086[10];  // R.xy G.xy B.xy W.xy maxLum minLum
  uint32_t hasCta861_3;
  float cta861_3[2];  // maxContentLightLevel maxFrameAverageLightLevel
  // SMPTE2094_10, SMPTE2094_40, SMPTE2094_50; len 0 = unset
  uint32_t blobLen[3];
  uint8_t blob[3][kMaxBlob];
};
static_assert(sizeof(SharedMetadata) <= kPageSize);

}  // namespace linuxapk
