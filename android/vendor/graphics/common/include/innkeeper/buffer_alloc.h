// SPDX-License-Identifier: AGPL-3.0-or-later
// Allocation of linear buffers: memfd -> udmabuf.
#pragma once

#include <cstdint>
#include <string_view>

#include <cutils/native_handle.h>

#include "innkeeper/layout.h"

namespace innkeeper {

struct AllocRequest {
  std::string_view name;
  uint32_t width, height, layerCount;
  int32_t format;
  uint64_t usage;
  uint64_t reservedSize;
};

enum class AllocStatus { Ok, BadDescriptor, Unsupported, NoResources };

// Checks the request without allocating.
AllocStatus checkRequest(const AllocRequest& req, Layout* outLayout = nullptr);

struct GpuBuffer {
  int fd = -1;
  uint64_t modifier = 0;
};
using GpuAllocate = GpuBuffer (*)(const AllocRequest&, Layout*);

// Allocates one buffer and returns a new native handle (caller owns fds +
// struct).
AllocStatus allocateBuffer(
  const AllocRequest& req, native_handle_t** outHandle,
  GpuAllocate gpuAllocate = nullptr
);

// True if the last allocation produced a dma-buf (udmabuf available).
bool udmabufAvailable();

// Opens /dev/udmabuf (or $INNKEEPER_UDMABUF) now; later calls reuse the fd.
// The allocator calls this before dropping root, since in the session only
// root (the host user) can open the device. Returns false if unavailable.
bool openUdmabufDevice();

}  // namespace innkeeper
