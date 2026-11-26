// SPDX-License-Identifier: AGPL-3.0-or-later
// Host tests for the camera HAL's pixel work: scaling NV12 into each gralloc
// YCbCr layout with a centered crop, range expansion, and the JPEG encoder's
// stream with its Exif orientation and thumbnail. With an output path
// argument, it also writes a test JPEG for a decoder to check.
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

// Seeded noise, the hardest image for a JPEG encoder.
std::vector<uint8_t> noise(size_t n) {
  std::vector<uint8_t> v(n);
  uint32_t x = 12345;
  for (auto& b : v) {
    x = x * 1103515245 + 12345;
    b = uint8_t(x >> 16);
  }
  return v;
}

uint32_t be16(const uint8_t* p) { return uint32_t(p[0] << 8 | p[1]); }
uint32_t be32(const uint8_t* p) { return be16(p) << 16 | be16(p + 2); }

// The size in the first SOF0 at or after `from`.
bool sofSize(
  const std::vector<uint8_t>& j, size_t from, uint32_t* w, uint32_t* h
) {
  for (size_t i = from; i + 9 < j.size(); i++) {
    if (j[i] == 0xff && j[i + 1] == 0xc0) {
      *h = be16(&j[i + 5]);
      *w = be16(&j[i + 7]);
      return true;
    }
  }
  return false;
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

void expandsToFullRange() {
  // 4x2: Y black, white, below black and above white; then Cb, Cr.
  uint8_t img[] = {16, 235, 0, 255, 126, 125, 16, 240, 128, 128, 16, 240};
  expandRange(img, 4, 2);
  const uint8_t want[] = {0, 255, 0, 255, 128, 127, 0, 255, 128, 128, 0, 255};
  CHECK(memcmp(img, want, sizeof(want)) == 0);
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
  // A JFIF APP0, and SOF0 carries the size.
  CHECK(j[2] == 0xff && j[3] == 0xe0 && memcmp(&j[6], "JFIF", 5) == 0);
  uint32_t sw, sh;
  CHECK(sofSize(j, 0, &sw, &sh) && sw == w && sh == h);
  // Lower quality makes a smaller file.
  CHECK(encodeJpeg(i420.data(), w, h, 10).size() < j.size());
  if (path) {
    FILE* f = fopen(path, "wb");
    fwrite(j.data(), 1, j.size(), f);
    fclose(f);
  }
}

void exifSegmentBytes() {
  // Orientation 90 is TIFF's 6, and IFD1 points at the thumbnail.
  const std::vector<uint8_t> thumb = {0xff, 0xd8, 1, 2, 3, 0xff, 0xd9};
  auto e = exifSegment(90, thumb);
  CHECK(e.size() == 74 + thumb.size());
  CHECK(memcmp(e.data(), "Exif\0\0MM\0*", 10) == 0);
  const uint8_t* t = e.data() + 6;
  CHECK(be32(t + 4) == 8);
  CHECK(be16(t + 8) == 1);
  CHECK(be16(t + 10) == 0x0112 && be16(t + 12) == 3 && be16(t + 18) == 6);
  CHECK(be32(t + 22) == 26);
  CHECK(be16(t + 26) == 3);
  CHECK(be16(t + 28) == 0x0103 && be16(t + 36) == 6);
  CHECK(be16(t + 40) == 0x0201 && be32(t + 48) == 68);
  CHECK(be16(t + 52) == 0x0202 && be32(t + 60) == thumb.size());
  CHECK(be32(t + 64) == 0);
  CHECK(memcmp(t + 68, thumb.data(), thumb.size()) == 0);
  // The other quarter turns. Without a thumbnail, IFD0 ends the chain.
  e = exifSegment(180, {});
  CHECK(e.size() == 32 && be16(&e[24]) == 3 && be32(&e[28]) == 0);
  CHECK(be16(&exifSegment(270, {})[24]) == 8);
  CHECK(be16(&exifSegment(0, {})[24]) == 1);
  // A thumbnail too large for the segment is left out.
  std::vector<uint8_t> most(kMaxThumbnailBytes), over(kMaxThumbnailBytes + 1);
  CHECK(exifSegment(0, most).size() == 65533);
  CHECK(exifSegment(0, over).size() == 32);
}

// The Exif payload of a captureJpeg result, and its thumbnail.
void exifOf(
  const std::vector<uint8_t>& j, std::vector<uint8_t>* exif,
  std::vector<uint8_t>* thumb
) {
  CHECK(j[2] == 0xff && j[3] == 0xe1);
  exif->assign(j.begin() + 6, j.begin() + 4 + be16(&j[4]));
  CHECK(memcmp(exif->data(), "Exif\0\0", 6) == 0);
  thumb->clear();
  const uint8_t* t = exif->data() + 6;
  uint32_t ifd1 = be32(t + 22);
  if (!ifd1) return;
  uint32_t off = be32(t + ifd1 + 22), n = be32(t + ifd1 + 34);
  CHECK(6 + off + n <= exif->size());
  thumb->assign(t + off, t + off + n);
}

void captureWithThumbnail() {
  // A 64x48 capture of a 128x48 frame, with a 32x24 thumbnail.
  auto f = noise(128 * 48 * 3 / 2);
  JpegParams p;
  p.orientation = 270;
  p.thumbWidth = 32;
  p.thumbHeight = 24;
  auto j = captureJpeg({f.data(), 128, 48}, 64, 48, p);
  std::vector<uint8_t> exif, thumb;
  exifOf(j, &exif, &thumb);
  CHECK(be16(&exif[6 + 18]) == 8);
  CHECK(thumb.size() > 4 && thumb[0] == 0xff && thumb[1] == 0xd8);
  CHECK(thumb[thumb.size() - 2] == 0xff && thumb.back() == 0xd9);
  uint32_t w, h;
  CHECK(sofSize(thumb, 0, &w, &h) && w == 32 && h == 24);
  CHECK(sofSize(j, 4 + be16(&j[4]), &w, &h) && w == 64 && h == 48);
  // No thumbnail asked for, none written.
  p.thumbWidth = p.thumbHeight = 0;
  exifOf(captureJpeg({f.data(), 128, 48}, 64, 48, p), &exif, &thumb);
  CHECK(thumb.empty());
  // A noisy 320x240 thumbnail at quality 100 is far over Exif's 64 KiB, so
  // it is encoded at a lower quality.
  auto big = noise(640 * 480 * 3 / 2);
  p.thumbWidth = 320;
  p.thumbHeight = 240;
  p.thumbQuality = 100;
  exifOf(captureJpeg({big.data(), 640, 480}, 640, 480, p), &exif, &thumb);
  CHECK(!thumb.empty() && thumb.size() <= kMaxThumbnailBytes);
}

}  // namespace

int main(int argc, char** argv) {
  sameSizeIsACopy();
  planarWithStrides();
  cropsToTheAspectRatio();
  expandsToFullRange();
  jpegStream(argc > 1 ? argv[1] : nullptr);
  exifSegmentBytes();
  captureWithThumbnail();
  return 0;
}
