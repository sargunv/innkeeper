// Host-only stand-in for the NDK's <android/rect.h>.
#pragma once
#include <stdint.h>
typedef struct ARect {
  int32_t left, top, right, bottom;
} ARect;
