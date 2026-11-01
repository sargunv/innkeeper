// SPDX-License-Identifier: AGPL-3.0-or-later
// Pixel format table and linear buffer layout. This is plain C++ with no
// Android dependencies, so it can be unit-tested on the host.
#pragma once

#include <cstdint>

namespace innkeeper {

// Values match android.hardware.graphics.common (PixelFormat,
// PlaneLayoutComponentType, ChromaSiting, BufferUsage).
namespace fmt {
constexpr int32_t RGBA_8888 = 0x1, RGBX_8888 = 0x2, RGB_888 = 0x3,
                  RGB_565 = 0x4, BGRA_8888 = 0x5, YCRCB_420_SP = 0x11,
                  RGBA_FP16 = 0x16, RAW16 = 0x20, BLOB = 0x21,
                  IMPLEMENTATION_DEFINED = 0x22, YCBCR_420_888 = 0x23,
                  RGBA_1010102 = 0x2B, Y8 = 0x20203859, Y16 = 0x20363159,
                  YV12 = 0x32315659, YCBCR_P010 = 0x36, R_8 = 0x38,
                  R_16_UINT = 0x39, RG_1616_UINT = 0x3a;
}
namespace comp {
constexpr int64_t Y = 1, CB = 2, CR = 4, R = 1 << 10, G = 1 << 11, B = 1 << 12,
                  RAW = 1 << 20, A = 1 << 30;
}
namespace usage {
constexpr uint64_t CPU_READ_MASK = 0xf, CPU_WRITE_MASK = 0xf0,
                   GPU_TEXTURE = 1 << 8, GPU_RENDER_TARGET = 1 << 9,
                   COMPOSER_OVERLAY = 1 << 11, COMPOSER_CLIENT_TARGET = 1 << 12,
                   PROTECTED = 1 << 14, VIDEO_ENCODER = 1 << 16,
                   CAMERA_OUTPUT = 1 << 17, CAMERA_INPUT = 1 << 18,
                   VIDEO_DECODER = 1 << 22, GPU_DATA_BUFFER = 1 << 24,
                   COMPOSER_CURSOR = 1 << 15, GPU_CUBE_MAP = 1 << 25,
                   GPU_MIPMAP_COMPLETE = 1 << 26;
// Usages whose consumers, such as GPU drivers, import the buffer as a
// dma-buf. These buffers must not get the memfd fallback.
constexpr uint64_t NEEDS_DMABUF =
  GPU_TEXTURE | GPU_RENDER_TARGET | COMPOSER_OVERLAY | COMPOSER_CLIENT_TARGET |
  COMPOSER_CURSOR | GPU_DATA_BUFFER | GPU_CUBE_MAP | GPU_MIPMAP_COMPLETE;
}  // namespace usage
enum class ChromaSiting : int64_t {
  NONE = 0,
  UNKNOWN = 1,
  SITED_INTERSTITIAL = 2
};

constexpr uint32_t fourcc(char a, char b, char c, char d) {
  return uint32_t(uint8_t(a)) | uint32_t(uint8_t(b)) << 8 |
         uint32_t(uint8_t(c)) << 16 | uint32_t(uint8_t(d)) << 24;
}

constexpr uint32_t kMaxPlanes = 3;
constexpr uint32_t kMaxComponents = 4;
constexpr uint64_t kPageSize = 4096;

struct Component {
  int64_t type;
  int32_t offsetBits;
  int32_t sizeBits;
};

struct PlaneFormat {
  uint32_t sampleIncrementBits;  // bits between consecutive samples in a row
  uint32_t hSub, vSub;           // subsampling factors (1 or 2)
  uint32_t numComponents;
  Component comps[kMaxComponents];
};

struct FormatInfo {
  int32_t format;   // Android PixelFormat
  uint32_t fourcc;  // DRM fourcc of the linear layout
  bool yuv;
  uint32_t numPlanes;
  PlaneFormat planes[kMaxPlanes];
};

// Returns nullptr if the format is unsupported. resolveFormat() maps
// IMPLEMENTATION_DEFINED and YCBCR_420_888 to a concrete format.
const FormatInfo* formatInfo(int32_t format);
// Maps flexible formats to a concrete one (0 if unsupported).
int32_t resolveFormat(int32_t format, uint64_t usage);
const char* formatName(int32_t format);

struct PlaneLayout {
  uint64_t offset;  // bytes from start of the layer
  uint32_t stride;  // bytes per row
  uint64_t size;    // bytes
  uint32_t widthInSamples, heightInSamples;
};

struct Layout {
  int32_t format;          // requested
  int32_t resolvedFormat;  // concrete
  uint32_t fourcc;
  uint32_t width, height, layerCount;
  uint32_t numPlanes;
  PlaneLayout planes[kMaxPlanes];
  uint32_t stridePixels;  // plane-0 stride in samples (AllocationResult.stride
                          // / STRIDE)
  uint64_t layerSize;     // bytes per layer (page aligned)
  uint64_t pixelBytes;    // layerSize * layerCount
};

// Returns false for unsupported format/size. Every plane's rows are padded to
// 256-byte multiples (except BLOB) so GPU drivers can import the buffer as
// linear. For YV12 that means a 512-byte luma stride.
bool computeLayout(
  int32_t format, uint32_t width, uint32_t height, uint32_t layerCount,
  uint64_t usage, Layout* out
);

constexpr uint64_t alignUp(uint64_t v, uint64_t a) {
  return (v + a - 1) / a * a;
}

}  // namespace innkeeper
