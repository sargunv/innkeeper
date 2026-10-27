// Force-included in host (glibc) builds of code that uses AOSP binder NDK
// headers. binder_status.h defines a statement-like __assert() macro on
// non-Bionic hosts, which breaks glibc's (unguarded) __assert declaration.
// Routing it to an overloaded function keeps both the declaration and the
// binder headers' calls valid.
#pragma once
#ifdef __cplusplus
extern "C" void linuxapk_host_assert(const char*, const char*, int) noexcept
        __attribute__((noreturn));  // shape of glibc's __assert declaration
[[noreturn]] void linuxapk_host_assert(const char* file, int line, const char* msg) noexcept;
#define __assert(a, b, c) linuxapk_host_assert(a, b, c)
#endif
