// SPDX-License-Identifier: AGPL-3.0-or-later
// android.hardware.graphics.allocator.IAllocator/default (AIDL V3).
// Buffers are linear memfd-backed dma-bufs; see common/buffer_alloc.cpp.
#define LOG_TAG "linuxapk-allocator"

#include <aidl/android/hardware/graphics/allocator/AllocationError.h>
#include <aidl/android/hardware/graphics/allocator/BnAllocator.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <unistd.h>

#include <cstring>
#include <string>

#include "linuxapk/buffer_alloc.h"
#include "linuxapk/gralloc_handle.h"
#include "linuxapk/log.h"
#include "linuxapk/native_handle.h"

using aidl::android::hardware::common::NativeHandle;
using aidl::android::hardware::graphics::allocator::AllocationError;
using aidl::android::hardware::graphics::allocator::AllocationResult;
using aidl::android::hardware::graphics::allocator::BnAllocator;
using aidl::android::hardware::graphics::allocator::BufferDescriptorInfo;

namespace linuxapk {
namespace {

ndk::ScopedAStatus error(AllocationError e) {
    return ndk::ScopedAStatus::fromServiceSpecificError(static_cast<int32_t>(e));
}

ndk::ScopedAStatus toStatus(AllocStatus s) {
    switch (s) {
        case AllocStatus::Ok: return ndk::ScopedAStatus::ok();
        case AllocStatus::BadDescriptor: return error(AllocationError::BAD_DESCRIPTOR);
        case AllocStatus::Unsupported: return error(AllocationError::UNSUPPORTED);
        case AllocStatus::NoResources: return error(AllocationError::NO_RESOURCES);
    }
    return error(AllocationError::NO_RESOURCES);
}

AllocRequest toRequest(const BufferDescriptorInfo& d, std::string* nameStorage) {
    const auto* n = reinterpret_cast<const char*>(d.name.data());
    *nameStorage = std::string(n, strnlen(n, d.name.size()));
    return AllocRequest{*nameStorage,
                        uint32_t(d.width > 0 ? d.width : 0),
                        uint32_t(d.height > 0 ? d.height : 0),
                        uint32_t(d.layerCount > 0 ? d.layerCount : 0),
                        static_cast<int32_t>(d.format),
                        static_cast<uint64_t>(d.usage),
                        uint64_t(d.reservedSize > 0 ? d.reservedSize : 0)};
}

// Moves the fds into the AIDL handle and frees the native handle.
NativeHandle toAidl(native_handle_t* h) {
    NativeHandle out;
    for (int i = 0; i < h->numFds; i++) out.fds.emplace_back(h->data[i]);
    out.ints.assign(&h->data[h->numFds], &h->data[h->numFds + h->numInts]);
    nativeHandleDelete(h);
    return out;
}

class Allocator : public BnAllocator {
  public:
    ndk::ScopedAStatus allocate(const std::vector<uint8_t>&, int32_t, AllocationResult*) override {
        // IMapper 4 descriptors are not supported; clients use allocate2 with IMapper 5.
        return error(AllocationError::UNSUPPORTED);
    }

    ndk::ScopedAStatus allocate2(const BufferDescriptorInfo& desc, int32_t count,
                                 AllocationResult* result) override {
        if (count <= 0 || count > 64) return error(AllocationError::BAD_DESCRIPTOR);
        std::string name;
        AllocRequest req = toRequest(desc, &name);
        if (desc.reservedSize < 0) return error(AllocationError::BAD_DESCRIPTOR);
        Layout layout;
        AllocStatus st = checkRequest(req, &layout);
        if (st != AllocStatus::Ok) {
            LOGW("allocate2 %s %dx%d fmt=0x%x usage=0x%llx: rejected (%d)", name.c_str(),
                 desc.width, desc.height, int(desc.format),
                 (unsigned long long)desc.usage, int(st));
            return toStatus(st);
        }
        AllocationResult r;
        r.stride = int32_t(layout.stridePixels);
        for (int32_t i = 0; i < count; i++) {
            native_handle_t* h = nullptr;
            st = allocateBuffer(req, &h);
            if (st != AllocStatus::Ok) return toStatus(st);  // r's fds close on return
            r.buffers.push_back(toAidl(h));
        }
        LOGD("allocate2 %s %dx%d fmt=0x%x usage=0x%llx x%d stride=%d", name.c_str(),
             desc.width, desc.height, int(desc.format), (unsigned long long)desc.usage, count,
             r.stride);
        *result = std::move(r);
        return ndk::ScopedAStatus::ok();
    }

    ndk::ScopedAStatus isSupported(const BufferDescriptorInfo& desc, bool* out) override {
        std::string name;
        *out = desc.reservedSize >= 0 && checkRequest(toRequest(desc, &name)) == AllocStatus::Ok;
        return ndk::ScopedAStatus::ok();
    }

    ndk::ScopedAStatus getIMapperLibrarySuffix(std::string* out) override {
        *out = "linuxapk";
        return ndk::ScopedAStatus::ok();
    }

    ndk::ScopedAStatus isMultiViewSupported(const std::vector<BufferDescriptorInfo>&, int32_t,
                                            bool* out) override {
        *out = false;
        return ndk::ScopedAStatus::ok();
    }

    ndk::ScopedAStatus allocateMultiView(const std::vector<BufferDescriptorInfo>&, int32_t,
                                         AllocationResult*) override {
        return error(AllocationError::UNSUPPORTED);
    }
};

}  // namespace
}  // namespace linuxapk

int main() {
    ABinderProcess_setThreadPoolMaxThreadCount(4);
    ABinderProcess_startThreadPool();
    auto allocator = ndk::SharedRefBase::make<linuxapk::Allocator>();
    const std::string instance = std::string(linuxapk::Allocator::descriptor) + "/default";
    binder_status_t st = AServiceManager_addService(allocator->asBinder().get(), instance.c_str());
    if (st != STATUS_OK) {
        LOGE("failed to register %s: %d", instance.c_str(), st);
        return 1;
    }
    LOGI("registered %s (V%d, mapper suffix linuxapk)", instance.c_str(),
         linuxapk::Allocator::version);
    ABinderProcess_joinThreadPool();
    return 1;
}
