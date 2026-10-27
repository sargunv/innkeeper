// SPDX-License-Identifier: AGPL-3.0-or-later
// Rate-limited dumps of composed frames to PNG/PPM, for headless inspection.
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
bool writePpm(const std::string& path, const FrameView& f);

class FrameDumper {
 public:
  struct Config {
    std::string dir;             // empty: disabled
    std::string format = "png";  // "png" or "ppm"
    int64_t intervalMs = 1000;   // minimum time between dumps
    uint32_t keep =
      0;  // 0: only <prefix>-latest; N: also rotate N numbered files
  };
  // Reads vendor.linuxapk.hwc.dump.{dir,format,interval_ms,keep}.
  static Config configFromProperties();

  explicit FrameDumper(Config c) : mConfig(std::move(c)) {}
  bool enabled() {
    std::lock_guard lock(mMutex);
    return !mConfig.dir.empty();
  }
  // Re-reads the properties (at most once per second), so dumping can be
  // switched on with setprop while the composer runs.
  void reloadFromProperties(int64_t nowNs);
  // True if a frame presented at nowNs should be dumped (and books it).
  bool shouldDump(int64_t nowNs);
  // Writes <dir>/<prefix>-latest.<ext> atomically (tmp + rename).
  bool dump(const std::string& prefix, const FrameView& f);

 private:
  Config mConfig;
  std::mutex mMutex;
  int64_t mLastNs = INT64_MIN / 2;
  int64_t mLastReloadNs = INT64_MIN / 2;
  uint64_t mCount = 0;
};

}  // namespace linuxapk::hwc
