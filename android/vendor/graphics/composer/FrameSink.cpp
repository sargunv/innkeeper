// SPDX-License-Identifier: AGPL-3.0-or-later
#include <android/hardware_buffer_jni.h>
#include <dlfcn.h>
#include <jni.h>
#include <sys/mman.h>

#include "innkeeper/gralloc_handle.h"

extern "C" JNIEXPORT jlong JNICALL
Java_dev_sargunv_innkeeper_system_wm_FrameSink_presentationToken(
  JNIEnv* env, jobject, jobject hardwareBuffer
) {
  // AHardwareBuffer_getId identifies the GraphicBuffer wrapper, which differs
  // from the gralloc allocation that HWC receives. This LL-NDK system API
  // exposes the allocation's native handle without mapping the image's pixels.
  using GetHandle = const native_handle_t* (*)(const AHardwareBuffer*);
  static auto getHandle = reinterpret_cast<GetHandle>(
    dlsym(RTLD_DEFAULT, "AHardwareBuffer_getNativeHandle")
  );
  auto* buffer = AHardwareBuffer_fromHardwareBuffer(env, hardwareBuffer);
  const auto* handle = getHandle && buffer
                         ? innkeeper::asBufferHandle(getHandle(buffer))
                         : nullptr;
  if (!handle) return 0;
  void* p = mmap(
    nullptr, innkeeper::kPageSize, PROT_READ, MAP_SHARED, handle->metadataFd, 0
  );
  if (p == MAP_FAILED) return 0;
  auto* md = static_cast<const innkeeper::SharedMetadata*>(p);
  uint64_t token = __atomic_load_n(&md->presentationToken, __ATOMIC_ACQUIRE);
  munmap(p, innkeeper::kPageSize);
  return jlong(token);
}
