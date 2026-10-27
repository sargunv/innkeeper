// SPDX-License-Identifier: AGPL-3.0-or-later
#include "linuxapk/layout.h"

#include <numeric>

namespace linuxapk {
namespace {

using namespace comp;

constexpr PlaneFormat packed(uint32_t bits, Component a, Component b = {}, Component c = {},
                             Component d = {}) {
    PlaneFormat p{bits, 1, 1, 0, {a, b, c, d}};
    p.numComponents = d.type ? 4 : c.type ? 3 : b.type ? 2 : 1;
    return p;
}

const FormatInfo kFormats[] = {
        {fmt::RGBA_8888, fourcc('A', 'B', '2', '4'), false, 1,
         {packed(32, {R, 0, 8}, {G, 8, 8}, {B, 16, 8}, {A, 24, 8})}},
        {fmt::RGBX_8888, fourcc('X', 'B', '2', '4'), false, 1,
         {packed(32, {R, 0, 8}, {G, 8, 8}, {B, 16, 8})}},
        {fmt::BGRA_8888, fourcc('A', 'R', '2', '4'), false, 1,
         {packed(32, {B, 0, 8}, {G, 8, 8}, {R, 16, 8}, {A, 24, 8})}},
        {fmt::RGB_888, fourcc('B', 'G', '2', '4'), false, 1,
         {packed(24, {R, 0, 8}, {G, 8, 8}, {B, 16, 8})}},
        {fmt::RGB_565, fourcc('R', 'G', '1', '6'), false, 1,
         {packed(16, {B, 0, 5}, {G, 5, 6}, {R, 11, 5})}},
        {fmt::RGBA_FP16, fourcc('A', 'B', '4', 'H'), false, 1,
         {packed(64, {R, 0, 16}, {G, 16, 16}, {B, 32, 16}, {A, 48, 16})}},
        {fmt::RGBA_1010102, fourcc('A', 'B', '3', '0'), false, 1,
         {packed(32, {R, 0, 10}, {G, 10, 10}, {B, 20, 10}, {A, 30, 2})}},
        {fmt::R_8, fourcc('R', '8', ' ', ' '), false, 1, {packed(8, {R, 0, 8})}},
        {fmt::R_16_UINT, fourcc('R', '1', '6', ' '), false, 1, {packed(16, {R, 0, 16})}},
        {fmt::RG_1616_UINT, fourcc('G', 'R', '3', '2'), false, 1,
         {packed(32, {R, 0, 16}, {G, 16, 16})}},
        {fmt::BLOB, fourcc('R', '8', ' ', ' '), false, 1, {packed(8, {RAW, 0, 8})}},
        {fmt::RAW16, fourcc('R', '1', '6', ' '), false, 1, {packed(16, {RAW, 0, 16})}},
        {fmt::Y8, fourcc('R', '8', ' ', ' '), true, 1, {packed(8, {Y, 0, 8})}},
        {fmt::Y16, fourcc('R', '1', '6', ' '), true, 1, {packed(16, {Y, 0, 16})}},
        // YCBCR_420_888 is allocated as NV12.
        {fmt::YCBCR_420_888, fourcc('N', 'V', '1', '2'), true, 2,
         {packed(8, {Y, 0, 8}), {16, 2, 2, 2, {{CB, 0, 8}, {CR, 8, 8}}}}},
        {fmt::YCRCB_420_SP, fourcc('N', 'V', '2', '1'), true, 2,
         {packed(8, {Y, 0, 8}), {16, 2, 2, 2, {{CR, 0, 8}, {CB, 8, 8}}}}},
        // YV12: Y, then Cr, then Cb (Android's documented layout).
        {fmt::YV12, fourcc('Y', 'V', '1', '2'), true, 3,
         {packed(8, {Y, 0, 8}), {8, 2, 2, 1, {{CR, 0, 8}}}, {8, 2, 2, 1, {{CB, 0, 8}}}}},
        {fmt::YCBCR_P010, fourcc('P', '0', '1', '0'), true, 2,
         {packed(16, {Y, 6, 10}), {32, 2, 2, 2, {{CB, 6, 10}, {CR, 22, 10}}}}},
};

constexpr uint64_t kRowAlign = 256;

uint32_t divUp(uint32_t v, uint32_t d) { return (v + d - 1) / d; }

}  // namespace

const FormatInfo* formatInfo(int32_t format) {
    for (const auto& f : kFormats) {
        if (f.format == format) return &f;
    }
    return nullptr;
}

int32_t resolveFormat(int32_t format, uint64_t u) {
    if (format == fmt::IMPLEMENTATION_DEFINED) {
        // Camera/video paths expect YUV; everything else gets RGBA.
        if (u & (usage::VIDEO_ENCODER | usage::VIDEO_DECODER | usage::CAMERA_OUTPUT |
                 usage::CAMERA_INPUT)) {
            return fmt::YCBCR_420_888;
        }
        return fmt::RGBA_8888;
    }
    return formatInfo(format) ? format : 0;
}

const char* formatName(int32_t format) {
    switch (format) {
        case fmt::RGBA_8888: return "RGBA_8888";
        case fmt::RGBX_8888: return "RGBX_8888";
        case fmt::BGRA_8888: return "BGRA_8888";
        case fmt::RGB_888: return "RGB_888";
        case fmt::RGB_565: return "RGB_565";
        case fmt::RGBA_FP16: return "RGBA_FP16";
        case fmt::RGBA_1010102: return "RGBA_1010102";
        case fmt::R_8: return "R_8";
        case fmt::R_16_UINT: return "R_16_UINT";
        case fmt::RG_1616_UINT: return "RG_1616_UINT";
        case fmt::BLOB: return "BLOB";
        case fmt::RAW16: return "RAW16";
        case fmt::Y8: return "Y8";
        case fmt::Y16: return "Y16";
        case fmt::YCBCR_420_888: return "YCBCR_420_888";
        case fmt::YCRCB_420_SP: return "YCRCB_420_SP";
        case fmt::YV12: return "YV12";
        case fmt::YCBCR_P010: return "YCBCR_P010";
        case fmt::IMPLEMENTATION_DEFINED: return "IMPLEMENTATION_DEFINED";
        default: return "?";
    }
}

bool computeLayout(int32_t format, uint32_t width, uint32_t height, uint32_t layerCount,
                   uint64_t usage, Layout* out) {
    if (width == 0 || height == 0 || layerCount == 0) return false;
    const int32_t resolved = resolveFormat(format, usage);
    if (resolved == fmt::BLOB) {
        // 1D buffers: width is the size in bytes (e.g. GPU data buffers).
        if (uint64_t(width) * height * layerCount > (1ull << 30)) return false;
    } else if (width > 32768 || height > 32768 || layerCount > 64) {
        return false;
    }
    const FormatInfo* fi = formatInfo(resolved);
    if (!fi) return false;

    Layout l{};
    l.format = format;
    l.resolvedFormat = resolved;
    l.fourcc = fi->fourcc;
    l.width = width;
    l.height = height;
    l.layerCount = layerCount;
    l.numPlanes = fi->numPlanes;

    const PlaneFormat& p0 = fi->planes[0];
    const uint32_t bpp0 = p0.sampleIncrementBits / 8;
    uint32_t stride0;
    if (resolved == fmt::BLOB) {
        stride0 = width;
    } else {
        // Smallest pixel multiple whose byte size is a multiple of kRowAlign.
        const uint32_t pixelAlign = kRowAlign / std::gcd<uint64_t, uint64_t>(kRowAlign, bpp0);
        stride0 = uint32_t(alignUp(width, pixelAlign)) * bpp0;
    }

    uint64_t offset = 0;
    for (uint32_t i = 0; i < fi->numPlanes; i++) {
        const PlaneFormat& pf = fi->planes[i];
        PlaneLayout& pl = l.planes[i];
        pl.widthInSamples = divUp(width, pf.hSub);
        pl.heightInSamples = divUp(height, pf.vSub);
        if (i == 0) {
            pl.stride = stride0;
        } else if (resolved == fmt::YV12) {
            pl.stride = uint32_t(alignUp(stride0 / 2, 16));  // Android YV12 rule
        } else {
            pl.stride = stride0;  // semi-planar chroma rows are as wide as luma rows
        }
        pl.offset = offset;
        pl.size = uint64_t(pl.stride) * pl.heightInSamples;
        offset += pl.size;
    }
    l.stridePixels = stride0 / (bpp0 ? bpp0 : 1);
    l.layerSize = alignUp(offset, kPageSize);
    l.pixelBytes = l.layerSize * layerCount;
    *out = l;
    return true;
}

}  // namespace linuxapk
