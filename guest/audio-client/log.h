// SPDX-License-Identifier: AGPL-3.0-or-later
// Logging for the audio client. Android builds log to logcat, builds with
// INNKEEPER_HILOG log to OpenHarmony's hilog, and host tests log nothing.
#pragma once

#if defined(__ANDROID__)
#include <android/log.h>
#define ALOG_(level, ...) \
  __android_log_print(ANDROID_LOG_##level, "innkeeper-audio", __VA_ARGS__)
#elif defined(INNKEEPER_HILOG)
#include <stdarg.h>
#include <stdio.h>
int HiLogPrint(
  int type, int level, unsigned int domain, const char* tag, const char* fmt,
  ...
);
enum { LXB_HILOG_INFO = 4, LXB_HILOG_WARN = 5, LXB_HILOG_ERROR = 6 };
// hilog hides unmarked arguments, so the line is formatted here and passed as
// one public string.
__attribute__((format(printf, 2, 3))) static inline void lxb_hilog(
  int level, const char* fmt, ...
) {
  char line[256];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof(line), fmt, ap);
  va_end(ap);
  HiLogPrint(3, level, 0xD0025FF, "innkeeper-audio", "%{public}s", line);
}
#define ALOG_(level, ...) lxb_hilog(LXB_HILOG_##level, __VA_ARGS__)
#else
#define ALOG_(level, ...) ((void)0)
#endif

#define ALOGI(...) ALOG_(INFO, __VA_ARGS__)
#define ALOGW(...) ALOG_(WARN, __VA_ARGS__)
#define ALOGE(...) ALOG_(ERROR, __VA_ARGS__)
