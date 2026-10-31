// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once
#include "linuxapk/buffer_alloc.h"
namespace linuxapk {
GpuBuffer allocateGpuBuffer(const AllocRequest& req, Layout* layout);
}
