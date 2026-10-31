// SPDX-License-Identifier: AGPL-3.0-or-later
// ABI-compatible stand-in for AOSP's <cutils/native_handle.h> (types only).
// Vendor code may not link libcutils; the helpers live in
// innkeeper/native_handle.h.
#pragma once

#include <stdalign.h>

#ifdef __cplusplus
extern "C" {
#endif

#define NATIVE_HANDLE_MAX_FDS 1024
#define NATIVE_HANDLE_MAX_INTS 1024

typedef struct native_handle {
  int version; /* sizeof(native_handle_t) */
  int numFds;
  int numInts;
#if defined(__clang__)
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wzero-length-array"
#pragma clang diagnostic ignored "-Wgnu-empty-struct"
#endif
  int data[0]; /* numFds fds followed by numInts ints */
#if defined(__clang__)
#pragma clang diagnostic pop
#endif
} native_handle_t;

typedef const native_handle_t* buffer_handle_t;

#ifdef __cplusplus
}
#endif
