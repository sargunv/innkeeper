// SPDX-License-Identifier: AGPL-3.0-or-later
#include <algorithm>
#include <cmath>

#include "Convert.h"

namespace innkeeper::camera {

void scaleNv12(const Nv12& src, const YuvTarget& dst) {
  if (!src.width || !src.height || !dst.width || !dst.height) return;
  // The largest centered source rectangle with the destination's aspect.
  uint64_t cw = src.width, ch = src.height;
  if (uint64_t(src.width) * dst.height > uint64_t(dst.width) * src.height) {
    cw = uint64_t(src.height) * dst.width / dst.height;
  } else {
    ch = uint64_t(src.width) * dst.height / dst.width;
  }
  cw = std::max<uint64_t>(cw, 1);
  ch = std::max<uint64_t>(ch, 1);
  const uint64_t x0 = (src.width - cw) / 2, y0 = (src.height - ch) / 2;
  const uint8_t* sy = src.data;
  const uint8_t* suv = src.data + size_t(src.width) * src.height;
  const size_t suvStride = size_t((src.width + 1) / 2) * 2;
  // Source column and row for each destination column and row.
  std::vector<uint32_t> xs(dst.width);
  for (uint32_t x = 0; x < dst.width; x++)
    xs[x] = uint32_t(x0 + (2 * x + 1) * cw / (2 * dst.width));
  for (uint32_t y = 0; y < dst.height; y++) {
    const uint8_t* row =
      sy + size_t(y0 + (2 * y + 1) * ch / (2 * dst.height)) * src.width;
    uint8_t* out = dst.y + y * dst.yStride;
    for (uint32_t x = 0; x < dst.width; x++) out[x] = row[xs[x]];
  }
  const uint32_t dcw = (dst.width + 1) / 2, dch = (dst.height + 1) / 2;
  for (uint32_t y = 0; y < dch; y++) {
    uint64_t srcY = y0 + (4 * y + 2) * ch / (2 * dst.height);
    const uint8_t* row =
      suv + size_t(std::min<uint64_t>(srcY, src.height - 1) / 2) * suvStride;
    uint8_t* cb = dst.cb + y * dst.cStride;
    uint8_t* cr = dst.cr + y * dst.cStride;
    for (uint32_t x = 0; x < dcw; x++) {
      uint32_t sx = xs[std::min(2 * x, dst.width - 1)] / 2;
      cb[x * dst.chromaStep] = row[2 * sx];
      cr[x * dst.chromaStep] = row[2 * sx + 1];
    }
  }
}

void expandRange(uint8_t* i420, uint32_t width, uint32_t height) {
  uint8_t luma[256], chroma[256];
  for (int v = 0; v < 256; v++) {
    int y = std::clamp(v, 16, 235), c = std::clamp(v, 16, 240);
    luma[v] = uint8_t(((y - 16) * 255 + 109) / 219);
    chroma[v] = uint8_t(((c - 16) * 255 + 112) / 224);
  }
  const size_t ys = size_t(width) * height;
  const size_t cs = 2 * size_t((width + 1) / 2) * ((height + 1) / 2);
  for (size_t i = 0; i < ys; i++) i420[i] = luma[i420[i]];
  for (size_t i = ys; i < ys + cs; i++) i420[i] = chroma[i420[i]];
}

namespace {

// ITU T.81 Annex K: quantization tables in natural order, Huffman tables.
constexpr uint8_t kLumaQ[64] = {
  16, 11, 10, 16, 24,  40,  51,  61,  12, 12, 14, 19, 26,  58,  60,  55,
  14, 13, 16, 24, 40,  57,  69,  56,  14, 17, 22, 29, 51,  87,  80,  62,
  18, 22, 37, 56, 68,  109, 103, 77,  24, 35, 55, 64, 81,  104, 113, 92,
  49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99
};
constexpr uint8_t kChromaQ[64] = {17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26,
                                  66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99,
                                  99, 99, 47, 66, 99, 99, 99, 99, 99, 99, 99,
                                  99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
                                  99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
                                  99, 99, 99, 99, 99, 99, 99, 99, 99};
constexpr uint8_t kZigzag[64] = {0,  1,  8,  16, 9,  2,  3,  10, 17, 24, 32,
                                 25, 18, 11, 4,  5,  12, 19, 26, 33, 40, 48,
                                 41, 34, 27, 20, 13, 6,  7,  14, 21, 28, 35,
                                 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23,
                                 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39,
                                 46, 53, 60, 61, 54, 47, 55, 62, 63};
constexpr uint8_t kDcLumaBits[16] = {0, 1, 5, 1, 1, 1, 1, 1,
                                     1, 0, 0, 0, 0, 0, 0, 0};
constexpr uint8_t kDcChromaBits[16] = {0, 3, 1, 1, 1, 1, 1, 1,
                                       1, 1, 1, 0, 0, 0, 0, 0};
constexpr uint8_t kDcVals[12] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11};
constexpr uint8_t kAcLumaBits[16] = {0, 2, 1, 3, 3, 2, 4, 3,
                                     5, 5, 4, 4, 0, 0, 1, 0x7d};
constexpr uint8_t kAcLumaVals[162] = {
  0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13,
  0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42,
  0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a,
  0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35,
  0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a,
  0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67,
  0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84,
  0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98,
  0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3,
  0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
  0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1,
  0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4,
  0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa
};
constexpr uint8_t kAcChromaBits[16] = {0, 2, 1, 2, 4, 4, 3, 4,
                                       7, 5, 4, 4, 0, 1, 2, 0x77};
constexpr uint8_t kAcChromaVals[162] = {
  0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51,
  0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1,
  0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24,
  0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a,
  0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
  0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66,
  0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82,
  0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96,
  0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa,
  0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
  0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9,
  0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4,
  0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa
};

struct Huffman {
  uint16_t code[256] = {};
  uint8_t size[256] = {};

  Huffman(const uint8_t* bits, const uint8_t* vals) {
    uint16_t c = 0;
    int k = 0;
    for (int len = 1; len <= 16; len++, c <<= 1) {
      for (int i = 0; i < bits[len - 1]; i++, k++, c++) {
        code[vals[k]] = c;
        size[vals[k]] = uint8_t(len);
      }
    }
  }
};

class Writer {
 public:
  std::vector<uint8_t> out;

  void byte(uint8_t b) { out.push_back(b); }
  void word(uint16_t w) {
    byte(uint8_t(w >> 8));
    byte(uint8_t(w));
  }
  void bits(uint32_t v, int n) {
    for (int i = n - 1; i >= 0; i--) {
      mAcc = uint8_t(mAcc << 1 | ((v >> i) & 1));
      if (++mCount == 8) {
        byte(mAcc);
        if (mAcc == 0xff) byte(0);
        mAcc = 0;
        mCount = 0;
      }
    }
  }
  // Pads the last byte with ones, as T.81 requires.
  void flush() {
    while (mCount) bits(1, 1);
  }

 private:
  uint8_t mAcc = 0;
  int mCount = 0;
};

void table(Writer& w, uint8_t cls, const uint8_t* bits, const uint8_t* vals) {
  int n = 0;
  for (int i = 0; i < 16; i++) n += bits[i];
  w.word(0xffc4);
  w.word(uint16_t(2 + 1 + 16 + n));
  w.byte(cls);
  for (int i = 0; i < 16; i++) w.byte(bits[i]);
  for (int i = 0; i < n; i++) w.byte(vals[i]);
}

struct Component {
  const Huffman* dc;
  const Huffman* ac;
  const uint8_t* q;
  int prev = 0;
};

// Category and extra bits of a coefficient (T.81 F.1.2).
void put(Writer& w, const Huffman& h, uint8_t symbolHigh, int v) {
  int a = v < 0 ? -v : v, n = 0;
  while (a >> n) n++;
  uint8_t s = uint8_t(symbolHigh | n);
  w.bits(h.code[s], h.size[s]);
  if (n) w.bits(uint32_t(v < 0 ? v + (1 << n) - 1 : v), n);
}

void block(Writer& w, Component& c, const float in[64]) {
  static float cosTable[8][8];
  static bool init = false;
  if (!init) {
    for (int x = 0; x < 8; x++)
      for (int u = 0; u < 8; u++)
        cosTable[x][u] = float(std::cos((2 * x + 1) * u * M_PI / 16));
    init = true;
  }
  int q[64];
  for (int v = 0; v < 8; v++) {
    for (int u = 0; u < 8; u++) {
      float s = 0;
      for (int y = 0; y < 8; y++)
        for (int x = 0; x < 8; x++)
          s += in[y * 8 + x] * cosTable[x][u] * cosTable[y][v];
      float cu = u ? 1.0f : float(M_SQRT1_2), cv = v ? 1.0f : float(M_SQRT1_2);
      s *= 0.25f * cu * cv;
      q[v * 8 + u] = int(std::lround(s / c.q[v * 8 + u]));
    }
  }
  put(w, *c.dc, 0, q[0] - c.prev);
  c.prev = q[0];
  int run = 0;
  for (int i = 1; i < 64; i++) {
    int v = q[kZigzag[i]];
    if (!v) {
      run++;
      continue;
    }
    while (run > 15) {
      w.bits(c.ac->code[0xf0], c.ac->size[0xf0]);
      run -= 16;
    }
    put(w, *c.ac, uint8_t(run << 4), v);
    run = 0;
  }
  if (run) w.bits(c.ac->code[0], c.ac->size[0]);
}

}  // namespace

std::vector<uint8_t> encodeJpeg(
  const uint8_t* i420, uint32_t width, uint32_t height, int quality,
  const std::vector<uint8_t>& app1
) {
  quality = std::clamp(quality, 1, 100);
  int scale = quality < 50 ? 5000 / quality : 200 - 2 * quality;
  uint8_t lq[64], cq[64];
  for (int i = 0; i < 64; i++) {
    lq[i] = uint8_t(std::clamp((kLumaQ[i] * scale + 50) / 100, 1, 255));
    cq[i] = uint8_t(std::clamp((kChromaQ[i] * scale + 50) / 100, 1, 255));
  }
  static const Huffman dcL(kDcLumaBits, kDcVals), acL(kAcLumaBits, kAcLumaVals),
    dcC(kDcChromaBits, kDcVals), acC(kAcChromaBits, kAcChromaVals);
  Writer w;
  w.out.reserve(size_t(width) * height / 2 + 1024);
  w.word(0xffd8);
  if (!app1.empty()) {
    w.word(0xffe1);
    w.word(uint16_t(2 + app1.size()));
    w.out.insert(w.out.end(), app1.begin(), app1.end());
  } else {
    // JFIF 1.01, no density, no thumbnail.
    const uint8_t app0[] = {'J', 'F', 'I', 'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0};
    w.word(0xffe0);
    w.word(2 + sizeof(app0));
    for (uint8_t b : app0) w.byte(b);
  }
  w.word(0xffdb);
  w.word(2 + 2 * 65);
  for (int t = 0; t < 2; t++) {
    w.byte(uint8_t(t));
    for (int i = 0; i < 64; i++) w.byte((t ? cq : lq)[kZigzag[i]]);
  }
  w.word(0xffc0);
  w.word(17);
  w.byte(8);
  w.word(uint16_t(height));
  w.word(uint16_t(width));
  w.byte(3);
  const uint8_t comps[3][3] = {{1, 0x22, 0}, {2, 0x11, 1}, {3, 0x11, 1}};
  for (auto& c : comps) {
    w.byte(c[0]);
    w.byte(c[1]);
    w.byte(c[2]);
  }
  table(w, 0x00, kDcLumaBits, kDcVals);
  table(w, 0x10, kAcLumaBits, kAcLumaVals);
  table(w, 0x01, kDcChromaBits, kDcVals);
  table(w, 0x11, kAcChromaBits, kAcChromaVals);
  w.word(0xffda);
  w.word(12);
  w.byte(3);
  for (uint8_t i = 1; i <= 3; i++) {
    w.byte(i);
    w.byte(i == 1 ? 0x00 : 0x11);
  }
  w.byte(0);
  w.byte(63);
  w.byte(0);

  const uint32_t cw = (width + 1) / 2, ch = (height + 1) / 2;
  const uint8_t* planes[3] = {
    i420, i420 + size_t(width) * height,
    i420 + size_t(width) * height + size_t(cw) * ch
  };
  Component c[3] = {{&dcL, &acL, lq}, {&dcC, &acC, cq}, {&dcC, &acC, cq}};
  float in[64];
  // Samples past the edge repeat the last row and column.
  auto load = [&](int p, uint32_t pw, uint32_t ph, uint32_t bx, uint32_t by) {
    for (uint32_t y = 0; y < 8; y++) {
      const uint8_t* row = planes[p] + size_t(std::min(by + y, ph - 1)) * pw;
      for (uint32_t x = 0; x < 8; x++)
        in[y * 8 + x] = float(row[std::min(bx + x, pw - 1)]) - 128.0f;
    }
  };
  for (uint32_t my = 0; my < height; my += 16) {
    for (uint32_t mx = 0; mx < width; mx += 16) {
      for (uint32_t b = 0; b < 4; b++) {
        load(0, width, height, mx + (b & 1) * 8, my + (b >> 1) * 8);
        block(w, c[0], in);
      }
      for (int p = 1; p < 3; p++) {
        load(p, cw, ch, mx / 2, my / 2);
        block(w, c[p], in);
      }
    }
  }
  w.flush();
  w.word(0xffd9);
  return std::move(w.out);
}

std::vector<uint8_t> exifSegment(
  int orientation, const std::vector<uint8_t>& thumbnail
) {
  const bool thumb =
    !thumbnail.empty() && thumbnail.size() <= kMaxThumbnailBytes;
  std::vector<uint8_t> e = {'E', 'x', 'i', 'f', 0, 0};
  auto u16 = [&](uint32_t v) {
    e.push_back(uint8_t(v >> 8));
    e.push_back(uint8_t(v));
  };
  auto u32 = [&](uint32_t v) {
    u16(v >> 16);
    u16(v & 0xffff);
  };
  // An IFD entry of one SHORT (type 3) or LONG (type 4).
  auto entry = [&](uint16_t tag, uint16_t type, uint32_t v) {
    u16(tag);
    u16(type);
    u32(1);
    if (type == 3) {
      u16(v);
      u16(0);
    } else {
      u32(v);
    }
  };
  // A big-endian TIFF header. Offsets count from it.
  e.insert(e.end(), {'M', 'M', 0, 42});
  u32(8);
  // IFD0 at 8: TIFF's orientation code for the rotation.
  const uint16_t codes[4] = {1, 6, 3, 8};
  u16(1);
  entry(0x0112, 3, codes[(orientation / 90) & 3]);
  u32(thumb ? 26 : 0);
  if (thumb) {
    // IFD1 at 26: the JPEG thumbnail that follows it at 68.
    u16(3);
    entry(0x0103, 3, 6);
    entry(0x0201, 4, 68);
    entry(0x0202, 4, uint32_t(thumbnail.size()));
    u32(0);
    e.insert(e.end(), thumbnail.begin(), thumbnail.end());
  }
  return e;
}

namespace {

// Scales `src` into a tightly packed width x height image, NV12 or I420.
std::vector<uint8_t> scaled(
  const Nv12& src, uint32_t width, uint32_t height, bool i420
) {
  const uint32_t cw = (width + 1) / 2, ch = (height + 1) / 2;
  std::vector<uint8_t> out(size_t(width) * height + 2 * size_t(cw) * ch);
  uint8_t* c = out.data() + size_t(width) * height;
  YuvTarget t{out.data(), c, c + 1, width, height, width, 2 * size_t(cw), 2};
  if (i420)
    t = {out.data(), c, c + size_t(cw) * ch, width, height, width, cw, 1};
  scaleNv12(src, t);
  return out;
}

}  // namespace

std::vector<uint8_t> captureJpeg(
  const Nv12& src, uint32_t width, uint32_t height, const JpegParams& p
) {
  std::vector<uint8_t> image = scaled(src, width, height, false);
  const Nv12 primary{image.data(), width, height};
  std::vector<uint8_t> thumb;
  if (p.thumbWidth && p.thumbHeight) {
    auto t = scaled(primary, p.thumbWidth, p.thumbHeight, true);
    expandRange(t.data(), p.thumbWidth, p.thumbHeight);
    for (int q = std::clamp(p.thumbQuality, 1, 100);; q /= 2) {
      thumb = encodeJpeg(t.data(), p.thumbWidth, p.thumbHeight, q);
      if (thumb.size() <= kMaxThumbnailBytes || q == 1) break;
    }
  }
  // At the same size, scaling only splits the chroma into planes.
  std::vector<uint8_t> i420 = scaled(primary, width, height, true);
  expandRange(i420.data(), width, height);
  return encodeJpeg(
    i420.data(), width, height, p.quality, exifSegment(p.orientation, thumb)
  );
}

}  // namespace innkeeper::camera
