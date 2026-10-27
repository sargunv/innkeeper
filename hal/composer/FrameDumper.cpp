// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include "FrameDumper.h"

#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <zlib.h>

#include <cmath>
#include <vector>

#include "Properties.h"
#include "linuxapk/layout.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {
namespace {

uint8_t halfToU8(uint16_t h) {
    const uint32_t sign = h >> 15, exp = (h >> 10) & 0x1f, man = h & 0x3ff;
    float v;
    if (exp == 0) v = std::ldexp(float(man), -24);
    else if (exp == 31) v = man ? 0.f : 1.f;
    else v = std::ldexp(float(man | 0x400), int(exp) - 25);
    if (sign) v = 0.f;
    v = v > 1.f ? 1.f : v;
    return uint8_t(v * 255.f + 0.5f);
}

void putBe32(std::vector<uint8_t>& out, uint32_t v) {
    out.push_back(v >> 24);
    out.push_back(v >> 16);
    out.push_back(v >> 8);
    out.push_back(v);
}

void putChunk(std::vector<uint8_t>& out, const char* type, const uint8_t* data, size_t len) {
    putBe32(out, uint32_t(len));
    size_t start = out.size();
    out.insert(out.end(), type, type + 4);
    out.insert(out.end(), data, data + len);
    uint32_t crc = uint32_t(crc32(0, out.data() + start, uInt(len + 4)));
    putBe32(out, crc);
}

bool writeFileAtomic(const std::string& path, const void* data, size_t len) {
    std::string tmp = path + ".tmp";
    FILE* f = fopen(tmp.c_str(), "wb");
    if (!f) {
        LOGW("dump: open %s: %s", tmp.c_str(), strerror(errno));
        return false;
    }
    bool ok = fwrite(data, 1, len, f) == len;
    ok = fclose(f) == 0 && ok;
    if (ok) ok = rename(tmp.c_str(), path.c_str()) == 0;
    if (!ok) remove(tmp.c_str());
    return ok;
}

}  // namespace

bool convertRowToRgb8(const FrameView& f, uint32_t y, uint8_t* rgb) {
    const uint8_t* row = f.data + size_t(y) * f.strideBytes;
    switch (f.format) {
        case fmt::RGBA_8888:
        case fmt::RGBX_8888:
            for (uint32_t x = 0; x < f.width; x++) memcpy(rgb + 3 * x, row + 4 * x, 3);
            return true;
        case fmt::BGRA_8888:
            for (uint32_t x = 0; x < f.width; x++) {
                rgb[3 * x] = row[4 * x + 2];
                rgb[3 * x + 1] = row[4 * x + 1];
                rgb[3 * x + 2] = row[4 * x];
            }
            return true;
        case fmt::RGB_888:
            memcpy(rgb, row, size_t(f.width) * 3);
            return true;
        case fmt::RGB_565:
            for (uint32_t x = 0; x < f.width; x++) {
                uint16_t p = uint16_t(row[2 * x] | row[2 * x + 1] << 8);
                rgb[3 * x] = uint8_t(((p >> 11) & 0x1f) * 255 / 31);
                rgb[3 * x + 1] = uint8_t(((p >> 5) & 0x3f) * 255 / 63);
                rgb[3 * x + 2] = uint8_t((p & 0x1f) * 255 / 31);
            }
            return true;
        case fmt::RGBA_1010102:
            for (uint32_t x = 0; x < f.width; x++) {
                uint32_t p;
                memcpy(&p, row + 4 * x, 4);
                rgb[3 * x] = uint8_t((p & 0x3ff) >> 2);
                rgb[3 * x + 1] = uint8_t(((p >> 10) & 0x3ff) >> 2);
                rgb[3 * x + 2] = uint8_t(((p >> 20) & 0x3ff) >> 2);
            }
            return true;
        case fmt::RGBA_FP16:
            for (uint32_t x = 0; x < f.width; x++) {
                uint16_t c[4];
                memcpy(c, row + 8 * x, 8);
                for (int i = 0; i < 3; i++) rgb[3 * x + i] = halfToU8(c[i]);
            }
            return true;
        default:
            return false;
    }
}

bool writePng(const std::string& path, const FrameView& f) {
    // Filter byte 0 + RGB per row, deflated with zlib.
    const size_t rowLen = 1 + size_t(f.width) * 3;
    std::vector<uint8_t> raw(rowLen * f.height);
    for (uint32_t y = 0; y < f.height; y++) {
        raw[y * rowLen] = 0;
        if (!convertRowToRgb8(f, y, &raw[y * rowLen + 1])) return false;
    }
    uLongf zlen = compressBound(uLong(raw.size()));
    std::vector<uint8_t> z(zlen);
    if (compress2(z.data(), &zlen, raw.data(), uLong(raw.size()), 1) != Z_OK) return false;

    std::vector<uint8_t> out = {0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n'};
    uint8_t ihdr[13] = {};
    for (int i = 0; i < 4; i++) {
        ihdr[i] = uint8_t(f.width >> (24 - 8 * i));
        ihdr[4 + i] = uint8_t(f.height >> (24 - 8 * i));
    }
    ihdr[8] = 8;  // bit depth
    ihdr[9] = 2;  // truecolour RGB
    putChunk(out, "IHDR", ihdr, sizeof ihdr);
    putChunk(out, "IDAT", z.data(), zlen);
    putChunk(out, "IEND", nullptr, 0);
    return writeFileAtomic(path, out.data(), out.size());
}

bool writePpm(const std::string& path, const FrameView& f) {
    char header[64];
    int n = snprintf(header, sizeof header, "P6\n%u %u\n255\n", f.width, f.height);
    std::vector<uint8_t> out(header, header + n);
    out.resize(n + size_t(f.width) * 3 * f.height);
    for (uint32_t y = 0; y < f.height; y++) {
        if (!convertRowToRgb8(f, y, &out[n + size_t(y) * f.width * 3])) return false;
    }
    return writeFileAtomic(path, out.data(), out.size());
}

FrameDumper::Config FrameDumper::configFromProperties() {
    Config c;
    c.dir = getProperty("vendor.linuxapk.hwc.dump.dir");
    c.format = getProperty("vendor.linuxapk.hwc.dump.format", "png");
    c.intervalMs = getIntProperty("vendor.linuxapk.hwc.dump.interval_ms", 1000);
    c.keep = uint32_t(getIntProperty("vendor.linuxapk.hwc.dump.keep", 0));
    return c;
}

void FrameDumper::reloadFromProperties(int64_t nowNs) {
    {
        std::lock_guard lock(mMutex);
        if (nowNs - mLastReloadNs < 1'000'000'000) return;
        mLastReloadNs = nowNs;
    }
    Config c = configFromProperties();
    std::lock_guard lock(mMutex);
    if (c.dir != mConfig.dir) {
        LOGI("dump: %s%s", c.dir.empty() ? "disabled" : "enabled, dir ", c.dir.c_str());
    }
    mConfig = std::move(c);
}

bool FrameDumper::shouldDump(int64_t nowNs) {
    std::lock_guard lock(mMutex);
    if (mConfig.dir.empty()) return false;
    if (nowNs - mLastNs < mConfig.intervalMs * 1000000) return false;
    mLastNs = nowNs;
    return true;
}

bool FrameDumper::dump(const std::string& prefix, const FrameView& f) {
    Config cfg;
    {
        std::lock_guard lock(mMutex);
        cfg = mConfig;
    }
    if (cfg.dir.empty()) return false;
    const bool ppm = cfg.format == "ppm";
    const char* ext = ppm ? "ppm" : "png";
    mkdir(cfg.dir.c_str(), 0775);
    std::string latest = cfg.dir + "/" + prefix + "-latest." + ext;
    bool ok = ppm ? writePpm(latest, f) : writePng(latest, f);
    if (!ok) {
        LOGW("dump: failed to write %s (format 0x%x)", latest.c_str(), f.format);
        return false;
    }
    uint64_t n;
    {
        std::lock_guard lock(mMutex);
        n = mCount++;
    }
    if (cfg.keep > 0) {
        char name[64];
        snprintf(name, sizeof name, "-%04llu.", (unsigned long long)(n % cfg.keep));
        std::string numbered = cfg.dir + "/" + prefix + name + ext;
        if (link(latest.c_str(), (numbered + ".tmp").c_str()) == 0) {
            rename((numbered + ".tmp").c_str(), numbered.c_str());
        }
    }
    if (n == 0) LOGI("dump: first frame written to %s", latest.c_str());
    return true;
}

}  // namespace linuxapk::hwc
