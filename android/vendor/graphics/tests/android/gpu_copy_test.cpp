// SPDX-License-Identifier: AGPL-3.0-or-later
// Real EGL imports: pixel correctness, cache reuse and display teardown while
// another binder thread presents. Run in the named bionic Flatpak harness.
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <thread>

#include <sys/mman.h>

#include "../../allocator/gpu_buffer.h"
#include "Buffer.h"
#include "GpuCopy.h"
#include "innkeeper/buffer_alloc.h"

using namespace innkeeper;
using namespace innkeeper::hwc;
#define CHECK(x)                              \
  do {                                        \
    if (!(x)) {                               \
      std::fprintf(stderr, "FAIL: %s\n", #x); \
      std::abort();                           \
    }                                         \
  } while (0)

static std::unique_ptr<BufferRef> buffer() {
  native_handle_t* handle = nullptr;
  CHECK(
    allocateBuffer(
      {"gpu-copy-test", 64, 64, 1, fmt::RGBA_8888, 0x100 | 0x200 | 0x30 | 3, 0},
      &handle
    ) == AllocStatus::Ok
  );
  return std::make_unique<BufferRef>(handle);
}

int main() {
  auto source = buffer();
  auto target = buffer();
  // Producer pixels are written by this test, never by the GPU-copy path.
  void* pixels = mmap(
    nullptr, source->info()->totalSize.get(), PROT_READ | PROT_WRITE,
    MAP_SHARED, source->info()->fd, 0
  );
  CHECK(pixels != MAP_FAILED);
  auto* rgba = static_cast<uint32_t*>(pixels);
  for (int i = 0; i < 64 * 64; ++i) rgba[i] = 0xff123456;
  munmap(pixels, source->info()->totalSize.get());
  GpuCopy live;
  CHECK(live.copy(*source, *target));
  CHECK(*reinterpret_cast<const uint32_t*>(target->map()) == 0xff123456);
  native_handle_t* gpuHandle = nullptr;
  CHECK(
    allocateBuffer(
      {"gpu-tiled-copy", 64, 64, 1, fmt::RGBA_8888,
       usage::GPU_TEXTURE | usage::GPU_RENDER_TARGET, 0},
      &gpuHandle, allocateGpuBuffer
    ) == AllocStatus::Ok
  );
  BufferRef tiled(gpuHandle);
  CHECK(live.copy(*source, tiled));
  CHECK(live.copy(tiled, *target));
  CHECK(*reinterpret_cast<const uint32_t*>(target->map()) == 0xff123456);
  std::printf(
    "ok: GPU target flags=0x%x copied through CPU-readable sink\n",
    tiled.info()->flags
  );
  std::atomic<bool> stop = false;
  std::atomic<int> failures = 0;
  auto other = buffer();
  std::thread presents([&] {
    while (!stop)
      if (!live.copy(*source, *target)) ++failures;
  });
  for (int i = 0; i < 100; ++i) {
    GpuCopy removed;
    if (!removed.copy(*source, *other)) ++failures;
    // The removed display is destroyed outside ComposerClient's lock.
  }
  stop = true;
  presents.join();
  CHECK(failures == 0);
  std::puts("ok: GPU pixels, repeated imports and concurrent display teardown");
}
