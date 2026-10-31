// SPDX-License-Identifier: AGPL-3.0-or-later
// The parts of OpenHarmony's display HDI that our VDIs exchange with the
// image's services, laid out as OH 7.0 builds them: drivers_peripheral's
// base/buffer_handle.h and drivers_interface's display IDL types
// (DisplayComposerType.idl, DisplayBufferType.idl). OH's C++ runtime is libc++
// with the std::__h namespace; the few std::string and std::vector fields
// here are only written in place (lxvec, lxstr), never through libc++
// headers, so this builds without OH's SDK.
#pragma once

#include <stddef.h>
#include <stdint.h>

#define HDF_SUCCESS 0
#define HDF_FAILURE (-1)
#define HDF_ERR_NOT_SUPPORT (-2)
#define HDF_ERR_INVALID_PARAM (-3)

// PixelFormat (DisplayComposerType.idl), the ones we allocate.
enum {
  PIXEL_FMT_RGB_565 = 3,
  PIXEL_FMT_RGBX_8888 = 11,
  PIXEL_FMT_RGBA_8888 = 12,
  PIXEL_FMT_RGB_888 = 13,
  PIXEL_FMT_BGRX_8888 = 19,
  PIXEL_FMT_BGRA_8888 = 20,
  PIXEL_FMT_YCBCR_420_SP = 24,
  PIXEL_FMT_YCRCB_420_SP = 25,
  PIXEL_FMT_YCBCR_420_P = 28,
  PIXEL_FMT_YCRCB_420_P = 29,
  PIXEL_FMT_RGBA_1010102 = 34,
};

enum { HBM_USE_ASSIGN_SIZE = 1 << 7 };

// CompositionType
enum { COMPOSITION_CLIENT = 0 };
// InterfaceType. render_service casts it into its own enum, which starts
// with an UNKNOW, and takes only its MIPI (10) as a built-in panel: the main
// screen. 10 is HDI's DISP_INTF_PANEL.
enum { DISP_INTF_BUILT_IN = 10 };
// DispPowerStatus
enum { POWER_STATUS_ON = 0 };

struct BufferHandle {
  int32_t fd;
  int32_t width;
  int32_t stride;  // bytes
  int32_t height;
  int32_t size;
  int32_t format;
  uint64_t usage;
  void* virAddr;
  uint64_t phyAddr;
  uint32_t reserveFds;
  uint32_t reserveInts;
  int32_t reserve[];  // reserveFds fds, then reserveInts ints
};

struct AllocInfo {
  uint32_t width;
  uint32_t height;
  uint64_t usage;
  uint32_t format;
  uint32_t expectedSize;
};

struct IRect {
  int32_t x, y, w, h;
};

struct DisplayModeInfo {
  int32_t width;
  int32_t height;
  uint32_t freshRate;
  int32_t id;
};

// A libc++ std::vector<T>: three pointers, storage from operator new.
template <typename T>
struct lxvec {
  T* begin;
  T* end;
  T* cap;
};

// A libc++ std::string (ABI v1, little-endian): 24 bytes. A short string
// (under 23 bytes) is stored inline: its length << 1, then the bytes and NUL.
struct lxstr {
  unsigned char raw[24];
};

struct DisplayCapability {
  lxstr name;
  int32_t type;
  uint32_t phyWidth;   // mm
  uint32_t phyHeight;  // mm
  uint32_t supportLayers;
  uint32_t virtualDispCount;
  bool supportWriteBack;
  uint32_t propertyCount;
  lxvec<void> props;  // PropertyObject; we report none
};
static_assert(offsetof(DisplayCapability, props) == 56, "OH layout");
static_assert(sizeof(DisplayCapability) == 80, "OH layout");

typedef void (*HotPlugCallback)(uint32_t devId, bool connected, void* data);
typedef void (*VBlankCallback)(unsigned int sequence, uint64_t ns, void* data);
