// SPDX-License-Identifier: AGPL-3.0-or-later
// Reimplementation of the few libcutils native_handle helpers we need.
#pragma once

#include <cutils/native_handle.h>

namespace linuxapk {

native_handle_t* nativeHandleCreate(int numFds, int numInts);
// Duplicates every fd. Returns nullptr on failure (nothing leaked).
native_handle_t* nativeHandleClone(const native_handle_t* h);
// Closes the fds (not the struct).
void nativeHandleClose(const native_handle_t* h);
// Frees the struct (not the fds).
void nativeHandleDelete(native_handle_t* h);
inline void nativeHandleCloseAndDelete(native_handle_t* h) {
    if (h) {
        nativeHandleClose(h);
        nativeHandleDelete(h);
    }
}

}  // namespace linuxapk
