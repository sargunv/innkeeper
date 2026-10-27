// SPDX-License-Identifier: AGPL-3.0-or-later
// Allocation of linear buffers: memfd -> udmabuf.
#pragma once

#include <cutils/native_handle.h>

#include <cstdint>
#include <string_view>

#include "linuxapk/layout.h"

namespace linuxapk {

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

// Allocates one buffer and returns a new native handle (caller owns fds + struct).
AllocStatus allocateBuffer(const AllocRequest& req, native_handle_t** outHandle);

// True if the last allocation produced a dma-buf (udmabuf available).
bool udmabufAvailable();

}  // namespace linuxapk
