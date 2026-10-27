// SPDX-License-Identifier: AGPL-3.0-or-later
#include "linuxapk/native_handle.h"

#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

namespace linuxapk {

native_handle_t* nativeHandleCreate(int numFds, int numInts) {
    if (numFds < 0 || numInts < 0 || numFds > NATIVE_HANDLE_MAX_FDS ||
        numInts > NATIVE_HANDLE_MAX_INTS) {
        return nullptr;
    }
    size_t size = sizeof(native_handle_t) + sizeof(int) * (numFds + numInts);
    auto* h = static_cast<native_handle_t*>(calloc(1, size));
    if (!h) return nullptr;
    h->version = sizeof(native_handle_t);
    h->numFds = numFds;
    h->numInts = numInts;
    return h;
}

native_handle_t* nativeHandleClone(const native_handle_t* src) {
    if (!src) return nullptr;
    native_handle_t* h = nativeHandleCreate(src->numFds, src->numInts);
    if (!h) return nullptr;
    for (int i = 0; i < src->numFds; i++) {
        h->data[i] = fcntl(src->data[i], F_DUPFD_CLOEXEC, 0);
        if (h->data[i] < 0) {
            h->numFds = i;
            nativeHandleClose(h);
            nativeHandleDelete(h);
            return nullptr;
        }
    }
    memcpy(&h->data[src->numFds], &src->data[src->numFds], sizeof(int) * src->numInts);
    return h;
}

void nativeHandleClose(const native_handle_t* h) {
    if (!h) return;
    for (int i = 0; i < h->numFds; i++) {
        if (h->data[i] >= 0) close(h->data[i]);
    }
}

void nativeHandleDelete(native_handle_t* h) { free(h); }

}  // namespace linuxapk
