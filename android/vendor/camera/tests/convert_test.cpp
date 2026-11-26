// SPDX-License-Identifier: AGPL-3.0-or-later
// Host tests for the camera HAL's pixel work: scaling NV12 into each gralloc
// YCbCr layout with a centered crop, and the JPEG encoder's stream. With an
// output path argument, it also writes a test JPEG for a decoder to check.
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "Convert.h"

#define CHECK(cond)                                                   \
  do {                                                                \
    if (!(cond)) {                                                    \
      std::fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond); \
      std::abort();                                                   \
    }                                                                 \
  } while (0)

using namespace innkeeper::camera;

namespace {

// An NV12 frame whose luma is its column and whose Cb and Cr are the chroma
// column and row plus 100 and 200.
std::vector<uint8_t> pattern(uint32_t w, uint32_t h) {
  std::vector<uint8_t> f(w * h + 2 * ((w + 1) / 2) * ((h + 1) / 2));
  for (uint32_t y = 0; y < h; y++)
    for (uint32_t x = 0; x < w; x++) f[y * w + x] = uint8_t(x);
  uint8_t* uv = f.data() + w * h;
  for (uint32_t y = 0; y < (h + 1) / 2; y++) {
    for (uint32_t x = 0; x < (w + 1) / 2; x++) {
      uv[y * ((w + 1) / 2) * 2 + 2 * x] = uint8_t(100 + x);
      uv[y * ((w + 1) / 2) * 2 + 2 * x + 1] = uint8_t(200 + y);
    }
  }
  return f;
}

void sameSizeIsACopy() {
  auto f = pattern(8, 4);
  std::vector<uint8_t> out(f.size());
  YuvTarget t{out.data(), out.data() + 32, out.data() + 33, 8, 4, 8, 8, 2};
  scaleNv12({f.data(), 8, 4}, t);
  CHECK(out == f);
  // NV21 swaps the chroma bytes.
  YuvTarget nv21{out.data(), out.data() + 33, out.data() + 32, 8, 4, 8, 8, 2};
  scaleNv12({f.data(), 8, 4}, nv21);
  CHECK(out[32] == 200 && out[33] == 100);
}

void planarWithStrides() {
  // YV12 at half size, with padded strides: Y, then Cr, then Cb.
  auto f = pattern(8, 4);
  const size_t ys = 16, cs = 8;
  std::vector<uint8_t> out(ys * 2 + cs * 1 * 2, 0xee);
  uint8_t* cr = out.data() + ys * 2;
  uint8_t* cb = cr + cs;
  YuvTarget t{out.data(), cb, cr, 4, 2, ys, cs, 1};
  scaleNv12({f.data(), 8, 4}, t);
  // Columns 1, 3, 5, 7 of the source, and its chroma columns 0 and 2.
  const uint8_t y0[] = {1, 3, 5, 7};
  CHECK(memcmp(out.data(), y0, 4) == 0);
  CHECK(out[4] == 0xee);
  CHECK(cb[0] == 100 && cb[1] == 102);
  CHECK(cr[0] == 201 && cr[1] == 201);
}

void cropsToTheAspectRatio() {
  // 16x4 into 4x4 keeps the middle 4x4 of the source.
  auto f = pattern(16, 4);
  std::vector<uint8_t> out(4 * 4 + 8);
  YuvTarget t{out.data(), out.data() + 16, out.data() + 17, 4, 4, 4, 4, 2};
  scaleNv12({f.data(), 16, 4}, t);
  const uint8_t row[] = {6, 7, 8, 9};
  for (int y = 0; y < 4; y++) CHECK(memcmp(out.data() + y * 4, row, 4) == 0);
  CHECK(out[16] == 103 && out[18] == 104);
  // Odd sizes stay inside the source and the destination.
  auto odd = pattern(5, 3);
  std::vector<uint8_t> o(3 * 3 + 2 * 2 * 2 + 1, 0xee);
  YuvTarget ot{o.data(), o.data() + 9, o.data() + 10, 3, 3, 3, 4, 2};
  scaleNv12({odd.data(), 5, 3}, ot);
  CHECK(o.back() == 0xee);
}

void jpegStream(const char* path) {
  const uint32_t w = 37, h = 21;
  std::vector<uint8_t> i420(w * h + 2 * 19 * 11);
  for (uint32_t y = 0; y < h; y++)
    for (uint32_t x = 0; x < w; x++) i420[y * w + x] = uint8_t(x * 6 + y * 4);
  memset(i420.data() + w * h, 90, 19 * 11);
  memset(i420.data() + w * h + 19 * 11, 200, 19 * 11);
  auto j = encodeJpeg(i420.data(), w, h, 90);
  CHECK(j.size() > 200);
  CHECK(j[0] == 0xff && j[1] == 0xd8);
  CHECK(j[j.size() - 2] == 0xff && j.back() == 0xd9);
  // SOF0 carries the size.
  bool found = false;
  for (size_t i = 0; i + 9 < j.size(); i++) {
    if (j[i] == 0xff && j[i + 1] == 0xc0) {
      CHECK((j[i + 5] << 8 | j[i + 6]) == int(h));
      CHECK((j[i + 7] << 8 | j[i + 8]) == int(w));
      found = true;
      break;
    }
  }
  CHECK(found);
  // Lower quality makes a smaller file.
  CHECK(encodeJpeg(i420.data(), w, h, 10).size() < j.size());
  if (path) {
    FILE* f = fopen(path, "wb");
    fwrite(j.data(), 1, j.size(), f);
    fclose(f);
  }
}

}  // namespace

int main(int argc, char** argv) {
  sameSizeIsACopy();
  planarWithStrides();
  cropsToTheAspectRatio();
  jpegStream(argc > 1 ? argv[1] : nullptr);
  return 0;
}
