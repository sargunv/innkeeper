// SPDX-License-Identifier: AGPL-3.0-or-later
// How the composer (composer.cpp) shows render_service's layers. The bridge
// shows one buffer as the window's frame, and video layers as subsurfaces
// under or over it (LAYERS in host/bridge/src/display/proto.rs).
#pragma once

#include <stdint.h>

#include "hdi.h"

constexpr uint32_t fourcc(char a, char b, char c, char d) {
  return uint32_t(a) | uint32_t(b) << 8 | uint32_t(c) << 16 | uint32_t(d) << 24;
}

constexpr uint32_t kNv12 = fourcc('N', 'V', '1', '2');

// OH formats name bytes in memory order, and DRM formats name little-endian
// words. A panel shows no alpha and render_service clears to transparent
// black, so the window treats a frame as opaque, unless video layers show
// under it through the holes that render_service leaves for them.
inline uint32_t drmFormat(int32_t format, bool alpha = false) {
  switch (format) {
    case PIXEL_FMT_RGBA_8888:
    case PIXEL_FMT_RGBX_8888:
      return alpha ? fourcc('A', 'B', '2', '4') : fourcc('X', 'B', '2', '4');
    case PIXEL_FMT_BGRA_8888:
    case PIXEL_FMT_BGRX_8888:
      return alpha ? fourcc('A', 'R', '2', '4') : fourcc('X', 'R', '2', '4');
    default:
      return 0;
  }
}

// A buffer to show, as a dup of its fd and its layout.
struct Target {
  int fd = -1;
  int32_t width, height, stride, format;
};

struct Layer {
  bool used = false;
  bool cursor = false;   // render_service's hardware cursor layer
  bool changed = false;  // reported as CLIENT by the next GetDisplayCompChange
  int32_t type = COMPOSITION_CLIENT;
  Target buffer;
  int fence = -1;  // the buffer's acquire fence
  IRect region = {}, crop = {};
  int32_t transform = 0;
  bool translucent = false;  // a global alpha below 255
  uint32_t z = 0;
};

constexpr int kMaxDeviceLayers = 8;

// The layers that the composer sends to the bridge. Everything else goes into
// the client target that render_service draws with GL.
struct Choice {
  // The layer shown as the frame, or -1 for the client target. render_service
  // then draws nothing for it on its composer thread.
  int frame = -1;
  // The video layers, bottom to top, with the part of the screen and of the
  // buffer that each shows. The first `below` show under the frame.
  int layers[kMaxDeviceLayers];
  IRect region[kMaxDeviceLayers], crop[kMaxDeviceLayers];
  int count = 0, below = 0;
};

inline bool same(const IRect& a, const IRect& b) {
  return a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h;
}

// Whether `x` can be the frame: an opaque device layer that fills the screen
// with a whole, untransformed buffer in a format the bridge takes.
// render_service's uni-render output is such a layer.
inline bool fillsScreen(const Layer& x, const IRect& screen) {
  const Target& b = x.buffer;
  const IRect whole = {0, 0, b.width, b.height};
  return x.type == COMPOSITION_DEVICE && b.fd >= 0 && drmFormat(b.format) &&
         x.transform == 0 && !x.translucent && same(x.region, screen) &&
         same(x.crop, whole);
}

// Clips a video layer's `region` to the screen and its `crop` by the same
// share. Returns whether any of it is on the screen, with a crop inside the
// buffer.
inline bool clip(
  IRect& region, IRect& crop, const IRect& screen, const Target& b
) {
  if (region.w <= 0 || region.h <= 0 || crop.w <= 0 || crop.h <= 0)
    return false;
  if (
    crop.x < 0 || crop.y < 0 || crop.x + crop.w > b.width ||
    crop.y + crop.h > b.height
  ) {
    return false;
  }
  const int64_t x0 = region.x > screen.x ? region.x : screen.x;
  const int64_t y0 = region.y > screen.y ? region.y : screen.y;
  const int64_t rx1 = int64_t(region.x) + region.w;
  const int64_t ry1 = int64_t(region.y) + region.h;
  const int64_t sx1 = int64_t(screen.x) + screen.w;
  const int64_t sy1 = int64_t(screen.y) + screen.h;
  const int64_t x1 = rx1 < sx1 ? rx1 : sx1, y1 = ry1 < sy1 ? ry1 : sy1;
  if (x1 <= x0 || y1 <= y0) return false;
  // A buffer edge maps to the nearest whole pixel, and the crop keeps at
  // least one.
  auto edge = [](int64_t at, int64_t from, int64_t len, int32_t c, int32_t cl) {
    return c + (2 * (at - from) * cl + len) / (2 * len);
  };
  const int64_t cx0 = edge(x0, region.x, region.w, crop.x, crop.w);
  const int64_t cy0 = edge(y0, region.y, region.h, crop.y, crop.h);
  int64_t cx1 = edge(x1, region.x, region.w, crop.x, crop.w);
  int64_t cy1 = edge(y1, region.y, region.h, crop.y, crop.h);
  if (cx1 <= cx0) cx1 = cx0 + 1;
  if (cy1 <= cy0) cy1 = cy0 + 1;
  if (cx1 > crop.x + crop.w || cy1 > crop.y + crop.h) return false;
  region = {int32_t(x0), int32_t(y0), int32_t(x1 - x0), int32_t(y1 - y0)};
  crop = {int32_t(cx0), int32_t(cy0), int32_t(cx1 - cx0), int32_t(cy1 - cy0)};
  return true;
}

// Whether `x` is a video layer that the bridge can show, an opaque,
// untransformed NV12 device layer.
inline bool isVideo(const Layer& x) {
  return x.type == COMPOSITION_DEVICE && x.buffer.fd >= 0 &&
         x.buffer.format == PIXEL_FMT_YCBCR_420_SP && x.transform == 0 &&
         !x.translucent;
}

// Chooses how to show `n` layers on `screen`. The composer shows the layers
// when one layer, apart from the cursor's, fills the screen and the
// others are video layers. Otherwise every layer goes into the client target.
inline Choice choose(const Layer* layers, int n, const IRect& screen) {
  Choice c;
  int frame = -1, others = 0;
  int video[kMaxDeviceLayers];
  int videos = 0;
  for (int i = 0; i < n; i++) {
    const Layer& x = layers[i];
    if (!x.used || x.cursor) continue;
    if (isVideo(x) && videos < kMaxDeviceLayers) {
      video[videos++] = i;
    } else {
      frame = i;
      others++;
    }
  }
  if (others != 1 || !fillsScreen(layers[frame], screen)) return c;
  // Bottom to top. Videos are few, so an insertion sort does.
  for (int i = 1; i < videos; i++) {
    for (int j = i; j > 0 && layers[video[j]].z < layers[video[j - 1]].z; j--) {
      const int t = video[j];
      video[j] = video[j - 1];
      video[j - 1] = t;
    }
  }
  for (int i = 0; i < videos; i++) {
    const Layer& x = layers[video[i]];
    IRect region = x.region, crop = x.crop;
    // A video entirely off the screen goes to the client target, which
    // draws nothing for it.
    if (!clip(region, crop, screen, x.buffer)) continue;
    c.layers[c.count] = video[i];
    c.region[c.count] = region;
    c.crop[c.count] = crop;
    c.count++;
    if (x.z < layers[frame].z) c.below++;
  }
  c.frame = frame;
  return c;
}
