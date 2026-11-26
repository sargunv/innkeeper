// SPDX-License-Identifier: AGPL-3.0-or-later
// Pixel work for the camera HAL, with no Android dependencies so the host
// tests can run it: scaling the bridge's NV12 frames into the YCbCr layouts
// of gralloc buffers, and encoding them as JPEG for BLOB streams.
#pragma once

#include <cstddef>
#include <cstdint>
#include <vector>

namespace innkeeper::camera {

// A tightly packed NV12 frame, as HostCamera delivers it.
struct Nv12 {
  const uint8_t* data;
  uint32_t width, height;
};

// A YCbCr 4:2:0 destination. Semi-planar layouts have chromaStep 2, with Cb
// and Cr one byte apart in either order. Planar layouts have chromaStep 1.
struct YuvTarget {
  uint8_t* y;
  uint8_t* cb;
  uint8_t* cr;
  uint32_t width, height;
  size_t yStride, cStride;
  size_t chromaStep;
};

// Scales `src` into `dst`, cropping the source's center to the destination's
// aspect ratio, as SCALER_CROPPING_TYPE_CENTER_ONLY describes. Nearest
// neighbour is enough for preview and code scanning.
void scaleNv12(const Nv12& src, const YuvTarget& dst);

// Expands a tightly packed I420 image from the limited range camera frames
// use (BT.601 Y 16 to 235, Cb and Cr 16 to 240) to the full range JPEG's
// JFIF colour space needs.
void expandRange(uint8_t* i420, uint32_t width, uint32_t height);

// Encodes a tightly packed, full-range I420 image as a baseline JFIF JPEG at
// `quality` (1 to 100).
std::vector<uint8_t> encodeJpeg(
  const uint8_t* i420, uint32_t width, uint32_t height, int quality
);

}  // namespace innkeeper::camera
