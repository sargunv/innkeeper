// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once
#include "innkeeper/buffer_alloc.h"
namespace innkeeper {
GpuBuffer allocateGpuBuffer(const AllocRequest& req, Layout* layout);
}
