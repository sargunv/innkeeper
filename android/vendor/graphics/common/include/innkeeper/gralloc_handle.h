// SPDX-License-Identifier: AGPL-3.0-or-later
// The buffer_handle_t layout shared by our allocator, mapper and composer.
//
// Two fds, as minigbm and Mesa's importers expect (planes first, metadata
// last):
//   fd          pixel data, [0, totalSize), layerCount layers. A dma-buf from
//               /dev/udmabuf (or a plain memfd if udmabuf is not available,
//               flagged by kFlagDmabuf being clear), or, for GPU-only RGB
//               targets, the Vulkan driver's exported allocation.
//   metadataFd  a sealed memfd:
//                 [0, kPageSize)                SharedMetadata
//                 [kPageSize, +reservedSize)    client reserved region
// Keeping metadata out of the pixel fd lets a driver's dedicated allocation
// (RADV's tiled images) be the whole pixel buffer. Planes sit at the
// offsets/strides below with the modifier in SharedMetadata (LINEAR unless
// kFlagTiled), so GPU drivers can import fd directly.
#pragma once

#include <cstddef>
#include <cstdint>

#include <cutils/native_handle.h>

#include "innkeeper/layout.h"

namespace innkeeper {

constexpr uint32_t kHandleMagic = 0x504b4e49;  // "INKP"
constexpr uint32_t kHandleVersion = 2;
constexpr uint32_t kFlagDmabuf = 1u << 0;
constexpr uint32_t kFlagTiled = 1u << 1;

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
  int metadataFd;
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
  U64 totalSize;  // size of the pixel fd
  U64 layerSize;
  U64 reservedSize;  // as requested; metadataFd rounds it up to whole pages
  uint32_t numPlanes;
  HandlePlane planes[kMaxPlanes];
};

constexpr int kHandleNumFds = 2;
constexpr uint64_t kReservedOffset = kPageSize;  // in metadataFd
constexpr int kHandleNumInts =
  int((sizeof(BufferHandle) - offsetof(BufferHandle, magic)) / sizeof(int));
static_assert(offsetof(BufferHandle, fd) == sizeof(native_handle_t));
static_assert(
  offsetof(BufferHandle, magic) ==
  sizeof(native_handle_t) + kHandleNumFds * sizeof(int)
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

// Mutable per-buffer metadata, stored in the buffer's metadataFd so every
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
  // Composer's lease of this sink image. Zero means it was not handed to
  // Wayland (or has been released). The token is unique for each frame;
  // access it atomically across the composer and the ImageReader consumer.
  alignas(8) uint64_t presentationToken;
  uint64_t modifier;  // DRM modifier; CPU-accessed buffers are LINEAR (zero).
};
static_assert(sizeof(SharedMetadata) <= kPageSize);

}  // namespace innkeeper
