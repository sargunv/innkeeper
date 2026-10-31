// SPDX-License-Identifier: AGPL-3.0-or-later
// libdisplay_composer_vdi_impl.z.so: OpenHarmony's composer VDI, the C
// functions the image's composer_host looks up by name
// (drivers_interface display/composer/common/include/
// display_vdi_adapter_interface.h). One display, 0, a phone-sized panel shown
// in one desktop window through the bridge's display service. Every layer is
// composed by render_service (CLIENT); each committed client target goes to
// the bridge as a dma-buf. A timer supplies vsync.
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "hdi.h"
#include "linuxapk/bridge_client.h"
#include "log.h"

#define LOG_TAG "linuxapk-composer"
#define EXPORT extern "C" __attribute__((visibility("default")))

namespace {

constexpr uint32_t kDisplay = 0;
constexpr uint32_t kRefresh = 60;
constexpr int64_t kPeriodNs = 1'000'000'000 / kRefresh;
constexpr const char* kAppId = "dev.linuxapk.ohos";
constexpr const char* kTitle = "OpenHarmony";

// Bridge display service (host/bridge/src/display/proto.rs).
constexpr uint16_t kBufferAdd = 0x0203, kBufferRemove = 0x0204;
constexpr uint16_t kFrame = 0x0205, kDisplayWindow = 0x0208;
constexpr uint16_t kEvRelease = 0x4201;

// The panel, and so the window: LINUXAPK_OHOS_DISPLAY,
// <width>x<height>@<dpi> (ohos/sandbox.sh, which gives DMS the same dpi).
struct Mode {
  uint32_t width = 720, height = 1280, dpi = 320;
};

Mode mode() {
  Mode m;
  unsigned w, h, d;
  const char* s = getenv("LINUXAPK_OHOS_DISPLAY");
  if (s && sscanf(s, "%ux%u@%u", &w, &h, &d) == 3 && w && h && d) m = {w, h, d};
  return m;
}

constexpr uint32_t fourcc(char a, char b, char c, char d) {
  return uint32_t(a) | uint32_t(b) << 8 | uint32_t(c) << 16 | uint32_t(d) << 24;
}

// OH formats name bytes in memory order; DRM's are little-endian words. A
// panel shows no alpha, and render_service clears to transparent black, so
// the window takes the target as opaque.
uint32_t drmFormat(int32_t format) {
  switch (format) {
    case PIXEL_FMT_RGBA_8888:
    case PIXEL_FMT_RGBX_8888:
      return fourcc('X', 'B', '2', '4');
    case PIXEL_FMT_BGRA_8888:
    case PIXEL_FMT_BGRX_8888:
      return fourcc('X', 'R', '2', '4');
    default:
      return 0;
  }
}

constexpr int kMaxLayers = 64;
struct Layer {
  bool used;
  bool changed;  // reported as CLIENT by the next GetDisplayCompChange
  int32_t type;
};

// The latest client target: a dup of its fd and its layout.
struct Target {
  int fd = -1;
  int32_t width, height, stride, format;
};

// One import of a client target in the bridge, keyed by the dma-buf's inode.
constexpr int kMaxBuffers = 4;
struct Imported {
  ino_t ino;
  uint32_t id;  // 0: free slot
  bool busy;    // shown, until the bridge's RELEASE
};

struct State {
  pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
  pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
  HotPlugCallback hotplug = nullptr;
  void* hotplugData = nullptr;
  VBlankCallback vblank = nullptr;
  void* vblankData = nullptr;
  bool vsync = false;
  bool threads = false;
  int32_t power = POWER_STATUS_ON;
  uint32_t backlight = 255;
  Layer layers[kMaxLayers] = {};
  Target client;
  int clientFence = -1;  // its acquire fence
  // The bridge connection, once its window is shown.
  int sock = -1;
  uint32_t nextTag = 1;
  uint32_t nextBuffer = 1;
  Imported imported[kMaxBuffers] = {};
};
State g;

int64_t nowNs() {
  timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return int64_t(ts.tv_sec) * 1'000'000'000 + ts.tv_nsec;
}

timespec deadline(int64_t fromNow) {
  const int64_t t = nowNs() + fromNow;
  return {time_t(t / 1'000'000'000), long(t % 1'000'000'000)};
}

template <typename T>
void assign(lxvec<T>& v, const T* src, size_t n) {
  if (size_t(v.cap - v.begin) < n) {
    ::operator delete(v.begin);
    v.begin = static_cast<T*>(::operator new(n * sizeof(T)));
    v.cap = v.begin + n;
  }
  if (n) memcpy(v.begin, src, n * sizeof(T));
  v.end = v.begin + n;
}

void setString(lxstr& s, const char* v) {
  const size_t n = strlen(v);  // < 23: stored inline
  memset(s.raw, 0, sizeof(s.raw));
  s.raw[0] = (unsigned char)(n << 1);
  memcpy(s.raw + 1, v, n);
}

// --- bridge ------------------------------------------------------------------

lxb_msg message(uint16_t type) {
  lxb_msg m;
  lxb_msg_init(&m, type, g.nextTag++);
  return m;
}

void forgetImports() {
  for (auto& b : g.imported) b = {};
  pthread_cond_broadcast(&g.changed);
}

// Connects, asks for the window, then handles RELEASE until the bridge goes.
void* bridgeLoop(void*) {
  lxb_backoff backoff{};
  for (;;) {
    if (const int ms = lxb_backoff_ms(&backoff); ms > 0) usleep(ms * 1000);
    const int sock = lxb_connect(LXB_SERVICE_DISPLAY, "ohos-composer");
    if (sock < 0) continue;
    lxb_msg m;
    lxb_msg_init(&m, kDisplayWindow, 1);
    const Mode d = mode();
    const uint32_t fields[] = {kDisplay, d.width, d.height, d.dpi};
    for (uint32_t v : fields) lxb_put_u32(&m, v);
    lxb_put_name(&m, kAppId);
    lxb_put_name(&m, kTitle);
    lxb_packet p;
    if (const int r = lxb_call(sock, &m, &p); r != 0 || p.status != 0) {
      LOGE("DISPLAY_WINDOW: %d", r ? r : p.status);
      if (r == 0) lxb_close_fds(&p);
      close(sock);
      continue;
    }
    LOGI("display %u shows in window %u", kDisplay, lxb_u32(p.body));
    lxb_close_fds(&p);
    pthread_mutex_lock(&g.mu);
    g.sock = sock;
    pthread_mutex_unlock(&g.mu);
    while (lxb_recv(sock, &p) == 0) {
      if (p.type == kEvRelease && p.len >= 8) {
        const uint32_t id = lxb_u32(p.body);
        const bool invalid = lxb_u32(p.body + 4) != 0;
        pthread_mutex_lock(&g.mu);
        for (auto& b : g.imported) {
          if (b.id != id) continue;
          if (invalid) b = {};
          b.busy = false;
        }
        pthread_cond_broadcast(&g.changed);
        pthread_mutex_unlock(&g.mu);
      } else if (p.type == LXB_REPLY && p.status != 0) {
        LOGW("request %u: %d", p.tag, p.status);
      }
      lxb_close_fds(&p);
    }
    LOGW("bridge gone");
    pthread_mutex_lock(&g.mu);
    g.sock = -1;
    forgetImports();
    pthread_mutex_unlock(&g.mu);
    close(sock);
  }
  return nullptr;
}

bool waitIdle(Imported* b, int64_t ns) {
  const timespec until = deadline(ns);
  while (b->busy && b->id) {
    if (pthread_cond_timedwait(&g.changed, &g.mu, &until) == ETIMEDOUT) break;
  }
  return !b->busy;
}

// Shows the client target `c` in the window. Called with g.mu held.
void present(const Target& c) {
  const uint32_t format = drmFormat(c.format);
  struct stat st;
  if (g.sock < 0 || c.fd < 0 || !format || fstat(c.fd, &st) != 0) return;
  Imported* b = nullptr;
  for (auto& i : g.imported) {
    if (i.id && i.ino == st.st_ino) b = &i;
  }
  if (!b) {
    // A new target: a free slot, else retire an idle import (render_service
    // reallocated its targets).
    for (auto& i : g.imported) {
      if (!b && !i.id) b = &i;
    }
    for (auto& i : g.imported) {
      if (!b && !i.busy) {
        lxb_msg m = message(kBufferRemove);
        lxb_put_u32(&m, i.id);
        lxb_send(g.sock, &m, -1);
        b = &i;
      }
    }
    if (!b) return;
    *b = {st.st_ino, g.nextBuffer++, false};
    lxb_msg m = message(kBufferAdd);
    const uint32_t fields[] = {
      b->id,
      kDisplay,
      uint32_t(c.width),
      uint32_t(c.height),
      format,
      uint32_t(c.stride),
      0
    };
    for (uint32_t v : fields) lxb_put_u32(&m, v);
    lxb_put_u64(&m, 0);  // LINEAR
    if (lxb_send(g.sock, &m, c.fd) != 0) {
      *b = {};
      return;
    }
  }
  // render_service may draw into a target as soon as Commit returns, so the
  // compositor must be done with it: at most a frame's wait.
  if (!waitIdle(b, 2 * kPeriodNs)) return;
  lxb_msg m = message(kFrame);
  const uint32_t fields[] = {kDisplay, b->id, 0, 0, 0, 0};
  for (uint32_t v : fields) lxb_put_u32(&m, v);
  if (lxb_send(g.sock, &m, -1) == 0) b->busy = true;
  for (auto& i : g.imported) {
    if (&i != b) waitIdle(&i, 2 * kPeriodNs);
  }
}

// --- vsync -------------------------------------------------------------------

void* vsyncLoop(void*) {
  uint32_t sequence = 0;
  int64_t next = nowNs();
  for (;;) {
    pthread_mutex_lock(&g.mu);
    while (!g.vsync || !g.vblank) pthread_cond_wait(&g.changed, &g.mu);
    const VBlankCallback cb = g.vblank;
    void* data = g.vblankData;
    pthread_mutex_unlock(&g.mu);
    const int64_t now = nowNs();
    next = next + kPeriodNs > now ? next + kPeriodNs : now + kPeriodNs;
    const timespec at = {
      time_t(next / 1'000'000'000), long(next % 1'000'000'000)
    };
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &at, nullptr) ==
           EINTR) {
    }
    cb(++sequence, uint64_t(next), data);
  }
  return nullptr;
}

void startThreads() {
  if (g.threads) return;
  g.threads = true;
  pthread_t t;
  pthread_create(&t, nullptr, bridgeLoop, nullptr);
  pthread_detach(t);
  pthread_create(&t, nullptr, vsyncLoop, nullptr);
  pthread_detach(t);
}

struct Lock {
  Lock() { pthread_mutex_lock(&g.mu); }
  ~Lock() { pthread_mutex_unlock(&g.mu); }
};

Layer* layer(uint32_t devId, uint32_t id) {
  if (devId != kDisplay || id >= kMaxLayers || !g.layers[id].used) {
    return nullptr;
  }
  return &g.layers[id];
}

}  // namespace

// --- display -----------------------------------------------------------------

EXPORT int32_t RegHotPlugCallback(HotPlugCallback cb, void* data) {
  {
    Lock l;
    g.hotplug = cb;
    g.hotplugData = data;
    startThreads();
  }
  const Mode d = mode();
  LOGI("display %u: %ux%u at %u dpi", kDisplay, d.width, d.height, d.dpi);
  if (cb) cb(kDisplay, true, data);
  return HDF_SUCCESS;
}

EXPORT int32_t GetDisplayCapability(uint32_t devId, DisplayCapability& info) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  setString(info.name, "linuxapk");
  info.type = DISP_INTF_BUILT_IN;
  const Mode d = mode();
  info.phyWidth = d.width * 254 / (d.dpi * 10);
  info.phyHeight = d.height * 254 / (d.dpi * 10);
  info.supportLayers = kMaxLayers;
  info.virtualDispCount = 0;
  info.supportWriteBack = false;
  info.propertyCount = 0;
  return HDF_SUCCESS;
}

EXPORT int32_t
GetDisplaySupportedModes(uint32_t devId, lxvec<DisplayModeInfo>& modes) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  const Mode d = mode();
  const DisplayModeInfo info = {
    int32_t(d.width), int32_t(d.height), kRefresh, 0
  };
  assign(modes, &info, 1);
  return HDF_SUCCESS;
}

EXPORT int32_t GetDisplayMode(uint32_t devId, uint32_t& modeId) {
  modeId = 0;
  return devId == kDisplay ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

EXPORT int32_t SetDisplayMode(uint32_t devId, uint32_t modeId) {
  return devId == kDisplay && modeId == 0 ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

EXPORT int32_t GetDisplayPowerStatus(uint32_t devId, int32_t& status) {
  Lock l;
  status = g.power;
  return devId == kDisplay ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

EXPORT int32_t SetDisplayPowerStatus(uint32_t devId, int32_t status) {
  Lock l;
  g.power = status;
  return devId == kDisplay ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

EXPORT int32_t GetDisplayBacklight(uint32_t devId, uint32_t& level) {
  Lock l;
  level = g.backlight;
  return devId == kDisplay ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

EXPORT int32_t SetDisplayBacklight(uint32_t devId, uint32_t level) {
  Lock l;
  g.backlight = level;
  return devId == kDisplay ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

EXPORT int32_t GetDisplayProperty(uint32_t, uint32_t, uint64_t&) {
  return HDF_ERR_NOT_SUPPORT;
}

EXPORT int32_t SetDisplayProperty(uint32_t, uint32_t, uint64_t) {
  return HDF_ERR_NOT_SUPPORT;
}

EXPORT int32_t PrepareDisplayLayers(uint32_t devId, bool& needFlushFb) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  Lock l;
  for (auto& layer : g.layers) {
    if (layer.used && layer.type != COMPOSITION_CLIENT) layer.changed = true;
  }
  needFlushFb = true;
  return HDF_SUCCESS;
}

EXPORT int32_t GetDisplayCompChange(
  uint32_t devId, lxvec<uint32_t>& layers, lxvec<int32_t>& types
) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  uint32_t ids[kMaxLayers];
  int32_t clients[kMaxLayers];
  size_t n = 0;
  {
    Lock l;
    for (uint32_t i = 0; i < kMaxLayers; i++) {
      if (!g.layers[i].used || !g.layers[i].changed) continue;
      g.layers[i].changed = false;
      g.layers[i].type = COMPOSITION_CLIENT;
      ids[n] = i;
      clients[n++] = COMPOSITION_CLIENT;
    }
  }
  assign(layers, ids, n);
  assign(types, clients, n);
  return HDF_SUCCESS;
}

EXPORT int32_t SetDisplayClientCrop(uint32_t, const IRect&) {
  return HDF_SUCCESS;
}

EXPORT int32_t SetDisplayClientBuffer(
  uint32_t devId, const BufferHandle& buffer, int32_t fence
) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  Lock l;
  if (g.client.fd >= 0) close(g.client.fd);
  if (g.clientFence >= 0) close(g.clientFence);
  g.client = {
    buffer.fd >= 0 ? fcntl(buffer.fd, F_DUPFD_CLOEXEC, 0) : -1, buffer.width,
    buffer.height, buffer.stride, buffer.format
  };
  g.clientFence = fence >= 0 ? fcntl(fence, F_DUPFD_CLOEXEC, 0) : -1;
  return HDF_SUCCESS;
}

EXPORT int32_t SetDisplayClientDamage(uint32_t, lxvec<IRect>&) {
  return HDF_SUCCESS;
}

EXPORT int32_t SetDisplayVsyncEnabled(uint32_t devId, bool enabled) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  Lock l;
  g.vsync = enabled;
  pthread_cond_broadcast(&g.changed);
  return HDF_SUCCESS;
}

EXPORT int32_t
RegDisplayVBlankCallback(uint32_t devId, VBlankCallback cb, void* data) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  Lock l;
  g.vblank = cb;
  g.vblankData = data;
  startThreads();
  pthread_cond_broadcast(&g.changed);
  return HDF_SUCCESS;
}

// render_service's screen manager gives up unless these register. We have no
// HWC events, refresh requests or idle vblanks to report.
EXPORT int32_t RegHwcEventCallback(void*, void*) { return HDF_SUCCESS; }
EXPORT int32_t RegRefreshCallback(void*, void*) { return HDF_SUCCESS; }
EXPORT int32_t RegDisplayVBlankIdleCallback(void*, void*) {
  return HDF_SUCCESS;
}

// Release fences: none. Commit returns only once the compositor is done with
// every target but the new one.
EXPORT int32_t GetDisplayReleaseFence(
  uint32_t devId, lxvec<uint32_t>& layers, lxvec<int32_t>& fences
) {
  assign<uint32_t>(layers, nullptr, 0);
  assign<int32_t>(fences, nullptr, 0);
  return devId == kDisplay ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

EXPORT int32_t CreateVirtualDisplay(uint32_t, uint32_t, int32_t&, uint32_t&) {
  return HDF_ERR_NOT_SUPPORT;
}

EXPORT int32_t DestroyVirtualDisplay(uint32_t) { return HDF_ERR_NOT_SUPPORT; }

EXPORT int32_t SetVirtualDisplayBuffer(uint32_t, const BufferHandle&, int32_t) {
  return HDF_ERR_NOT_SUPPORT;
}

EXPORT int32_t Commit(uint32_t devId, int32_t& fence) {
  fence = -1;
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  pthread_mutex_lock(&g.mu);
  const int acquire = g.clientFence;
  g.clientFence = -1;
  pthread_mutex_unlock(&g.mu);
  if (acquire >= 0) {
    pollfd p = {acquire, POLLIN, 0};
    if (poll(&p, 1, 1000) != 1) LOGW("client target fence timeout");
    close(acquire);
  }
  Lock l;
  present(g.client);
  return HDF_SUCCESS;
}

// --- layers ------------------------------------------------------------------

EXPORT int32_t CreateLayer(uint32_t devId, const void*, uint32_t& layerId) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  Lock l;
  for (uint32_t i = 0; i < kMaxLayers; i++) {
    if (g.layers[i].used) continue;
    g.layers[i] = {true, false, COMPOSITION_CLIENT};
    layerId = i;
    return HDF_SUCCESS;
  }
  return HDF_FAILURE;
}

EXPORT int32_t DestroyLayer(uint32_t devId, uint32_t layerId) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  *x = {};
  return HDF_SUCCESS;
}

EXPORT int32_t
SetLayerCompositionType(uint32_t devId, uint32_t layerId, int32_t type) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  x->type = type;
  return HDF_SUCCESS;
}

// render_service composes every layer itself, so their properties and
// buffers don't matter here.
#define LAYER_SETTER(name, ...)                                         \
  EXPORT int32_t name(uint32_t devId, uint32_t layerId, __VA_ARGS__) {  \
    Lock l;                                                             \
    return layer(devId, layerId) ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM; \
  }
LAYER_SETTER(SetLayerAlpha, const void*)
LAYER_SETTER(SetLayerRegion, const IRect&)
LAYER_SETTER(SetLayerCrop, const IRect&)
LAYER_SETTER(SetLayerZorder, uint32_t)
LAYER_SETTER(SetLayerPreMulti, bool)
LAYER_SETTER(SetLayerTransformMode, int32_t)
LAYER_SETTER(SetLayerDirtyRegion, const lxvec<IRect>&)
LAYER_SETTER(SetLayerVisibleRegion, lxvec<IRect>&)
LAYER_SETTER(SetLayerBuffer, const BufferHandle&, int32_t)
LAYER_SETTER(SetLayerBlendType, int32_t)
LAYER_SETTER(SetLayerMaskInfo, int32_t)
LAYER_SETTER(SetLayerColor, const void*)
