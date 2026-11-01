// SPDX-License-Identifier: AGPL-3.0-or-later
// Buffer allocation. CPU-visible buffers are linear memfds wrapped in
// udmabufs, and GPU-only targets can come from the gpuAllocate callback.
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

// Allocates one buffer and returns a new native handle. The caller owns its
// fds and the struct.
AllocStatus allocateBuffer(
  const AllocRequest& req, native_handle_t** outHandle,
  GpuAllocate gpuAllocate = nullptr
);

// True once any allocation has produced a udmabuf dma-buf.
bool udmabufAvailable();

// Opens /dev/udmabuf, or $INNKEEPER_UDMABUF, and keeps the fd for later calls.
// The allocator calls this before dropping root, since in the session only
// root (the host user) can open the device. Returns false if unavailable.
bool openUdmabufDevice();

}  // namespace innkeeper
