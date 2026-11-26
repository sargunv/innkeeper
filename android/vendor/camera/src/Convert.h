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

// Encodes a tightly packed, full-range I420 image as a baseline JPEG at
// `quality` (1 to 100). Its application segment is `app1` as APP1, or a JFIF
// APP0 if `app1` is empty.
std::vector<uint8_t> encodeJpeg(
  const uint8_t* i420, uint32_t width, uint32_t height, int quality,
  const std::vector<uint8_t>& app1 = {}
);

// The most bytes encodeJpeg writes for a width x height image at any quality
// and with any APP1 segment. 8-bit samples keep each coefficient within the
// categories Annex K's tables code, so their longest codes bound the scan.
size_t jpegMaxBytes(uint32_t width, uint32_t height);

// The largest thumbnail an Exif APP1 segment holds: a segment's 65533 bytes
// less exifSegment's 74.
inline constexpr size_t kMaxThumbnailBytes = 65533 - 74;

// The payload of an Exif APP1 segment: the orientation, as the clockwise
// rotation in degrees (0, 90, 180 or 270) that shows the image upright, and
// the JPEG `thumbnail` unless it is empty or over kMaxThumbnailBytes.
std::vector<uint8_t> exifSegment(
  int orientation, const std::vector<uint8_t>& thumbnail
);

// What a still capture asks for. A 0x0 thumbnail means none.
struct JpegParams {
  int quality = 90;
  int orientation = 0;
  uint32_t thumbWidth = 0, thumbHeight = 0;
  int thumbQuality = 90;
};

// Encodes limited-range `src` as a width x height JPEG, cropped and scaled as
// scaleNv12 does, with an Exif segment that carries the orientation and a
// thumbnail cropped from the image. A thumbnail too large for Exif at its
// quality is encoded at a lower one.
std::vector<uint8_t> captureJpeg(
  const Nv12& src, uint32_t width, uint32_t height, const JpegParams& p
);

}  // namespace innkeeper::camera
