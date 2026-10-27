// SPDX-License-Identifier: AGPL-3.0-or-later
// Rate-limited dumps of composed frames to PNG, for headless inspection.
#pragma once

#include <cstdint>
#include <mutex>
#include <string>

namespace linuxapk::hwc {

struct FrameView {
  const uint8_t* data;
  uint32_t width, height;
  uint32_t strideBytes;
  int32_t format;  // Android PixelFormat
};

// Converts one row to 8-bit RGB. Returns false for unsupported formats.
bool convertRowToRgb8(const FrameView& f, uint32_t y, uint8_t* rgbOut);
bool writePng(const std::string& path, const FrameView& f);

// Dumps to vendor.linuxapk.hwc.dump.dir (empty: off) at most once a second.
class FrameDumper {
 public:
  explicit FrameDumper(std::string dir) : mDir(std::move(dir)) {}
  static std::string dirFromProperty();

  bool enabled() {
    std::lock_guard lock(mMutex);
    return !mDir.empty();
  }
  // Re-reads the property (at most once per second), so dumping can be
  // switched on with setprop while the composer runs.
  void reloadFromProperty(int64_t nowNs);
  // True if a frame presented at nowNs should be dumped (and books it).
  bool shouldDump(int64_t nowNs);
  // Writes <dir>/<prefix>-latest.png atomically (tmp + rename).
  bool dump(const std::string& prefix, const FrameView& f);

 private:
  std::string mDir;
  std::mutex mMutex;
  int64_t mLastNs = INT64_MIN / 2;
  int64_t mLastReloadNs = INT64_MIN / 2;
  bool mWroteFirst = false;
};

}  // namespace linuxapk::hwc
