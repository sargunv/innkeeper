// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once
#include "Buffer.h"

namespace innkeeper::hwc {
// Mixed virtual-display composition renders into scratch storage. Copy it on
// the GPU into the ImageReader's sink, whose ownership can follow RELEASE.
class GpuCopy {
 public:
  GpuCopy();
  ~GpuCopy();
  bool copy(const BufferRef& source, const BufferRef& target);

 private:
  struct State;
  std::unique_ptr<State> mState;
};
}  // namespace innkeeper::hwc
