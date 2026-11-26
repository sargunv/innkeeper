// SPDX-License-Identifier: AGPL-3.0-or-later
// Camera metadata for the host camera: the static characteristics, the
// default request settings and the per-frame results. Entries are collected
// in a map and serialized with the LL-NDK libcamera_metadata, whose
// camera_metadata_t blob is what the AIDL CameraMetadata carries.
#pragma once

#include <cstdint>
#include <cstring>
#include <map>
#include <vector>

#include <system/camera_metadata.h>

namespace innkeeper::camera {

struct Size {
  int32_t width, height;
};

// The output sizes the HAL offers, largest first. The bridge streams the
// desktop camera near the largest configured size, and the HAL scales.
inline constexpr Size kSizes[] = {
  {1280, 720}, {640, 480}, {320, 240}, {176, 144}
};
inline constexpr int32_t kMinFps = 15, kMaxFps = 30;

// The pixel formats the HAL writes (android.hardware.graphics.common).
inline constexpr int32_t kFormatBlob = 0x21, kFormatImplDefined = 0x22,
                         kFormatYcbcr420 = 0x23;

class Metadata {
 public:
  template <typename T>
  void set(uint32_t tag, std::vector<T> values) {
    Entry& e = mEntries[tag];
    e.count = values.size();
    e.bytes.resize(values.size() * sizeof(T));
    if (!values.empty()) memcpy(e.bytes.data(), values.data(), e.bytes.size());
  }

  void setRaw(uint32_t tag, size_t count, const void* data, size_t bytes) {
    Entry& e = mEntries[tag];
    e.count = count;
    e.bytes.assign(
      static_cast<const uint8_t*>(data),
      static_cast<const uint8_t*>(data) + bytes
    );
  }

  // A serialized camera_metadata_t, or empty if a tag's type doesn't match.
  std::vector<uint8_t> serialize() const;

 private:
  struct Entry {
    size_t count;
    std::vector<uint8_t> bytes;
  };
  std::map<uint32_t, Entry> mEntries;
};

// The host camera's static characteristics.
Metadata characteristics();

// The default settings for a RequestTemplate, or empty if unsupported.
std::vector<uint8_t> defaultRequest(int32_t requestTemplate);

// Whether `blob` is a well-formed camera_metadata_t.
bool validMetadata(const std::vector<uint8_t>& blob);

// A capture result: the request's settings, the frame's timestamp and the
// 3A states of a fixed-focus camera that converged at once.
std::vector<uint8_t> captureResult(
  const std::vector<uint8_t>& settings, int64_t timestampNs
);

// The request's JPEG quality, or 90.
int jpegQuality(const std::vector<uint8_t>& settings);

}  // namespace innkeeper::camera
