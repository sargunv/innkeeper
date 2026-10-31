// SPDX-License-Identifier: AGPL-3.0-or-later
// Logging to OpenHarmony's hilog (libhilog.so's C API), formatted here so
// every argument is public (hilog hides unmarked arguments as <private>).
#pragma once

#include <stdarg.h>
#include <stdio.h>

extern "C" int HiLogPrint(
  int type, int level, unsigned int domain, const char* tag, const char* fmt,
  ...
);

namespace lxlog {
constexpr int kCore = 3;  // LOG_CORE
constexpr unsigned int kDomain = 0xD0025FF;
enum { kInfo = 4, kWarn = 5, kError = 6 };  // LogLevel

__attribute__((format(printf, 3, 4))) inline void print(
  int level, const char* tag, const char* fmt, ...
) {
  char line[256];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof(line), fmt, ap);
  va_end(ap);
  HiLogPrint(kCore, level, kDomain, tag, "%{public}s", line);
}
}  // namespace lxlog

#define LOGI(...) lxlog::print(lxlog::kInfo, LOG_TAG, __VA_ARGS__)
#define LOGW(...) lxlog::print(lxlog::kWarn, LOG_TAG, __VA_ARGS__)
#define LOGE(...) lxlog::print(lxlog::kError, LOG_TAG, __VA_ARGS__)
