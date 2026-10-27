// SPDX-License-Identifier: AGPL-3.0-or-later
// Writes a test pattern through FrameDumper in each supported format; the
// PNGs can be eyeballed (and are validated by `file`/python in build.sh).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <vector>

#include "FrameDumper.h"
#include "linuxapk/layout.h"

using namespace linuxapk;
using namespace linuxapk::hwc;

void linuxapk_host_assert(const char* file, int line, const char* msg) noexcept { abort(); }

int main(int argc, char** argv) {
    std::string dir = argc > 1 ? argv[1] : "/tmp/dump-test";
    const uint32_t w = 200, h = 120;
    int fails = 0;
    for (int32_t f : {fmt::RGBA_8888, fmt::BGRA_8888, fmt::RGB_565, fmt::RGBA_1010102,
                      fmt::RGBA_FP16}) {
        Layout l;
        computeLayout(f, w, h, 1, 0, &l);
        std::vector<uint8_t> buf(l.layerSize);
        const uint32_t bpp = l.planes[0].stride / l.stridePixels;
        for (uint32_t y = 0; y < h; y++) {
            for (uint32_t x = 0; x < w; x++) {
                uint8_t* p = &buf[y * l.planes[0].stride + x * bpp];
                // Red ramp left->right, green top->bottom, blue square in the middle.
                uint8_t r = uint8_t(x * 255 / (w - 1)), g = uint8_t(y * 255 / (h - 1));
                uint8_t b = (x > 75 && x < 125 && y > 35 && y < 85) ? 255 : 0;
                uint32_t v;
                switch (f) {
                    case fmt::RGBA_8888: p[0] = r; p[1] = g; p[2] = b; p[3] = 255; break;
                    case fmt::BGRA_8888: p[0] = b; p[1] = g; p[2] = r; p[3] = 255; break;
                    case fmt::RGB_565:
                        v = (r >> 3) << 11 | (g >> 2) << 5 | (b >> 3);
                        p[0] = uint8_t(v); p[1] = uint8_t(v >> 8);
                        break;
                    case fmt::RGBA_1010102:
                        v = (r << 2) | (g << 2) << 10 | (b << 2) << 20 | 3u << 30;
                        memcpy(p, &v, 4);
                        break;
                    case fmt::RGBA_FP16: {
                        // 0 or 1.0 (0x3c00) per channel; ramps quantised to on/off.
                        uint16_t c[4] = {uint16_t(r > 127 ? 0x3c00 : 0),
                                         uint16_t(g > 127 ? 0x3c00 : 0),
                                         uint16_t(b ? 0x3c00 : 0), 0x3c00};
                        memcpy(p, c, 8);
                        break;
                    }
                }
            }
        }
        FrameView v{buf.data(), w, h, l.planes[0].stride, f};
        FrameDumper::Config c;
        c.dir = dir;
        c.keep = 2;
        c.format = "png";
        FrameDumper d(c);
        bool ok = d.shouldDump(0) && !d.shouldDump(1) && d.dump(formatName(f), v);
        c.format = "ppm";
        FrameDumper d2(c);
        ok = ok && d2.dump(formatName(f), v);
        uint8_t rgb[3 * w];
        convertRowToRgb8(v, 60, rgb);
        // Pixel (100,60) is inside the blue square, with red ~127 and green ~128.
        bool pixOk = rgb[300 + 2] == 255 && (f == fmt::RGBA_FP16 || (rgb[300] > 110 && rgb[300] < 140));
        printf("%-14s png+ppm %s, pixel(100,60)=%u,%u,%u %s\n", formatName(f), ok ? "ok" : "FAIL",
               rgb[300], rgb[301], rgb[302], pixOk ? "ok" : "FAIL");
        fails += !ok + !pixOk;
    }
    printf("wrote %s/*-latest.{png,ppm}\n", dir.c_str());
    return fails ? 1 : 0;
}
