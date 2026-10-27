// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include "Buffer.h"

#include <string.h>
#include <sys/mman.h>

#include "Fence.h"
#include "linuxapk/log.h"
#include "linuxapk/native_handle.h"

namespace linuxapk::hwc {

BufferRef::~BufferRef() {
    if (mMap) munmap(mMap, mMapSize);
    nativeHandleCloseAndDelete(mHandle);
}

const uint8_t* BufferRef::map() {
    if (mMap) return static_cast<const uint8_t*>(mMap);
    const BufferHandle* bh = info();
    if (!bh) return nullptr;
    void* p = mmap(nullptr, bh->totalSize.get(), PROT_READ, MAP_SHARED, bh->fd, 0);
    if (p == MAP_FAILED) {
        LOGW("mmap of buffer %llu failed: %s", (unsigned long long)bh->bufferId.get(),
             strerror(errno));
        return nullptr;
    }
    mMap = p;
    mMapSize = bh->totalSize.get();
    return static_cast<const uint8_t*>(mMap);
}

Error BufferSlots::update(const c3::Buffer& buffer, BufferPtr* out) {
    if (buffer.slot < 0 || buffer.slot > 4096) return Error::BadParameter;
    const size_t slot = size_t(buffer.slot);
    if (slot >= mSlots.size()) mSlots.resize(slot + 1);
    if (buffer.handle) {
        const auto& h = *buffer.handle;
        native_handle_t* nh = nativeHandleCreate(int(h.fds.size()), int(h.ints.size()));
        if (!nh) return Error::NoResources;
        for (size_t i = 0; i < h.fds.size(); i++) {
            nh->data[i] = dupFd(h.fds[i].get());
            if (nh->data[i] < 0) {
                nh->numFds = int(i);
                nativeHandleCloseAndDelete(nh);
                return Error::NoResources;
            }
        }
        if (!h.ints.empty()) {
            memcpy(&nh->data[h.fds.size()], h.ints.data(), h.ints.size() * sizeof(int));
        }
        mSlots[slot] = std::make_shared<BufferRef>(nh);
    }
    if (!mSlots[slot]) return Error::BadParameter;
    *out = mSlots[slot];
    return Error::None;
}

void BufferSlots::clear(int32_t slot) {
    if (slot >= 0 && size_t(slot) < mSlots.size()) mSlots[slot].reset();
}

}  // namespace linuxapk::hwc
