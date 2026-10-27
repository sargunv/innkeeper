// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#ifndef LOG_TAG
#define LOG_TAG "linuxapk-gralloc"
#endif

#if defined(__ANDROID__)
#include <android/log.h>
#include <stdio.h>
#include <stdlib.h>
// Logs go to logd; with LINUXAPK_LOG_STDERR=1 in the environment they are
// also mirrored to stderr (useful when testing without logd).
static inline bool linuxapk_log_stderr() {
    static const bool v = getenv("LINUXAPK_LOG_STDERR") != nullptr;
    return v;
}
#define LINUXAPK_LOG(prio, c, ...)                                                    \
    do {                                                                              \
        __android_log_print(prio, LOG_TAG, __VA_ARGS__);                              \
        if (linuxapk_log_stderr()) {                                                  \
            fprintf(stderr, c " " LOG_TAG ": " __VA_ARGS__);                          \
            fputc('\n', stderr);                                                      \
        }                                                                             \
    } while (0)
#define LOGD(...) LINUXAPK_LOG(ANDROID_LOG_DEBUG, "D", __VA_ARGS__)
#define LOGI(...) LINUXAPK_LOG(ANDROID_LOG_INFO, "I", __VA_ARGS__)
#define LOGW(...) LINUXAPK_LOG(ANDROID_LOG_WARN, "W", __VA_ARGS__)
#define LOGE(...) LINUXAPK_LOG(ANDROID_LOG_ERROR, "E", __VA_ARGS__)
#else
#include <stdio.h>
#define LOGD(...) ((void)0)
#define LOGI(...) (fprintf(stderr, "I " LOG_TAG ": " __VA_ARGS__), fputc('\n', stderr))
#define LOGW(...) (fprintf(stderr, "W " LOG_TAG ": " __VA_ARGS__), fputc('\n', stderr))
#define LOGE(...) (fprintf(stderr, "E " LOG_TAG ": " __VA_ARGS__), fputc('\n', stderr))
#endif
