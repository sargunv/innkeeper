// SPDX-License-Identifier: AGPL-3.0-or-later
// libdisplay_composer_vdi_impl.z.so is OpenHarmony's composer VDI. It exports
// the C functions that the image's composer_host looks up by name
// (drivers_interface display/composer/common/include/
// display_vdi_adapter_interface.h).
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "hdi.h"
#include "innkeeper/bridge_client.h"
#include "layers.h"
#include "log.h"

// From libbegetutil (base/startup/init interfaces/innerkits/include/
// param/parameter.h). Returns the value's length, or a negative error.
extern "C" int GetParameter(
  const char* key, const char* def, char* value, uint32_t len
);

// linux/dma-buf.h
struct dma_buf_sync {
  uint64_t flags;
};
constexpr uint64_t DMA_BUF_SYNC_READ = 1, DMA_BUF_SYNC_START = 0;
constexpr uint64_t DMA_BUF_SYNC_END = 1 << 2;
constexpr unsigned long DMA_BUF_IOCTL_SYNC = _IOW('b', 0, dma_buf_sync);

#define LOG_TAG "innkeeper-composer"
#define EXPORT extern "C" __attribute__((visibility("default")))

namespace {

constexpr uint32_t kDisplay = 0;
constexpr uint32_t kRefresh = 60;
constexpr int64_t kPeriodNs = 1'000'000'000 / kRefresh;
constexpr const char* kAppId = "dev.sargunv.innkeeper.ohos";
constexpr const char* kTitle = "OpenHarmony";

// Bridge display service (host/bridge/src/display/proto.rs).
constexpr uint16_t kBufferAdd = 0x0203, kBufferRemove = 0x0204;
constexpr uint16_t kFrame = 0x0205, kCursor = 0x0206, kDisplayWindow = 0x0208;
constexpr uint16_t kLayers = 0x0209;
constexpr uint16_t kEvRelease = 0x4201;

// The panel mode, which also sizes the window. It comes from the
// const.innkeeper.display parameter, <width>x<height>@<dpi>, which the
// launcher sets from the profile's display (host/launcher/src/ohos.rs). The
// launcher also gives the dpi to DMS. The panel's physical size in
// millimetres comes from const.innkeeper.display.size, <width>x<height>.
struct Mode {
  uint32_t width = 720, height = 1280, dpi = 320;
  uint32_t mmWidth = 0, mmHeight = 0;
};

Mode mode() {
  Mode m;
  unsigned w, h, d;
  char s[64] = "";
  if (
    GetParameter("const.innkeeper.display", "", s, sizeof(s)) > 0 &&
    sscanf(s, "%ux%u@%u", &w, &h, &d) == 3 && w && h && d
  )
    m = {w, h, d};
  if (
    GetParameter("const.innkeeper.display.size", "", s, sizeof(s)) > 0 &&
    sscanf(s, "%ux%u", &w, &h) == 2 && w && h
  ) {
    m.mmWidth = w;
    m.mmHeight = h;
  }
  return m;
}

// MMI draws its hardware cursor into a buffer of this size, with the
// pointer's position at the centre (ScreenPointer in multimodalinput).
constexpr int kCursorSize = 512;

constexpr int kMaxLayers = 64;

// One import of a buffer in the bridge, keyed by the dma-buf's inode and the
// format that the bridge shows it in. Frames and video layers each have a
// table.
constexpr int kMaxFrames = 4, kMaxVideoBuffers = 16;
struct Imported {
  ino_t ino;
  uint32_t format;
  uint32_t id;  // 0 marks a free slot
  bool busy;    // set while shown, until the bridge's RELEASE
};

// What Commit last showed: the frame, and the video layers bottom to top
// with the first `below` under the frame.
struct Shown {
  Target frame;
  Target layer[kMaxDeviceLayers];
  IRect region[kMaxDeviceLayers], crop[kMaxDeviceLayers];
  int count = 0, below = 0;
};

// The LAYERS request last sent.
struct SentLayers {
  uint32_t id[kMaxDeviceLayers];
  IRect region[kMaxDeviceLayers], crop[kMaxDeviceLayers];
  int count = 0, below = 0;
};

struct State {
  pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
  pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
  VBlankCallback vblank = nullptr;
  void* vblankData = nullptr;
  bool vsync = false;
  bool threads = false;
  int32_t power = POWER_STATUS_ON;
  uint32_t backlight = 255;
  Layer layers[kMaxLayers] = {};
  Target client;
  int clientFence = -1;  // its acquire fence
  Choice choice;         // the layers that Commit sends to the bridge
  // What Commit last showed. render_service draws into those buffers again
  // only after the next Commit, so a new window can show them.
  Shown shown;
  // The bridge connection, once its window is shown.
  int sock = -1;
  uint32_t nextTag = 1;
  uint32_t nextBuffer = 1;
  Imported frames[kMaxFrames] = {};
  Imported videos[kMaxVideoBuffers] = {};
  uint32_t sentFrame = 0;  // the import that the last FRAME showed
  SentLayers sentLayers;
  // MMI's cursor image as last read, and whether the bridge has it.
  uint32_t cursor[kCursorSize * kCursorSize];
  bool cursorSent = false;
};
State g;

int64_t nowNs() {
  timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return int64_t(ts.tv_sec) * 1'000'000'000 + ts.tv_nsec;
}

timespec at(int64_t ns) {
  return {time_t(ns / 1'000'000'000), long(ns % 1'000'000'000)};
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
  const size_t n = strlen(v);  // under 23, so stored inline
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
  for (auto& b : g.frames) b = {};
  for (auto& b : g.videos) b = {};
  g.sentFrame = 0;
  g.sentLayers = {};
  pthread_cond_broadcast(&g.changed);
}

void present(const Shown& s);

// Handles the bridge's RELEASE of import `id`, which `invalid` forgets.
void released(Imported* table, int n, uint32_t id, bool invalid) {
  for (int i = 0; i < n; i++) {
    if (table[i].id != id) continue;
    if (invalid) table[i] = {};
    table[i].busy = false;
  }
}

// Reconnects for the life of the process. Without a bridge the display is
// headless.
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
    // render_service commits a frame only when the screen changes, so a new
    // window gets the last frame here.
    present(g.shown);
    pthread_mutex_unlock(&g.mu);
    while (lxb_recv(sock, &p) == 0) {
      if (p.type == kEvRelease && p.len >= 8) {
        const uint32_t id = lxb_u32(p.body);
        const bool invalid = lxb_u32(p.body + 4) != 0;
        pthread_mutex_lock(&g.mu);
        released(g.frames, kMaxFrames, id, invalid);
        released(g.videos, kMaxVideoBuffers, id, invalid);
        pthread_cond_broadcast(&g.changed);
        pthread_mutex_unlock(&g.mu);
      } else if (p.type == LXB_REPLY && p.status != 0) {
        LOGW("request %u: %d", p.tag, p.status);
      }
      lxb_close_fds(&p);
    }
    LOGW("bridge gone");
    // The bridge ends the connection when the user closes or opens the
    // window, and the composer connects again at once. If the bridge has
    // stopped, that attempt fails and the next ones wait for the backoff.
    backoff = {};
    pthread_mutex_lock(&g.mu);
    g.sock = -1;
    g.cursorSent = false;
    forgetImports();
    pthread_mutex_unlock(&g.mu);
    close(sock);
  }
  return nullptr;
}

bool waitIdle(Imported* b, int64_t ns) {
  const timespec until = at(nowNs() + ns);
  while (b->busy && b->id) {
    if (pthread_cond_timedwait(&g.changed, &g.mu, &until) == ETIMEDOUT) break;
  }
  return !b->busy;
}

// Returns the import of `t` in `format` from `table`, adding it to the
// bridge if it is new. A new import takes a free slot, or else retires an
// idle import that `keep` doesn't hold, because render_service and decoders
// reallocate their buffers. Returns null if every slot is in use.
Imported* import(
  Imported* table, int n, const Target& t, uint32_t format,
  Imported* const* keep, int kept
) {
  struct stat st;
  if (t.fd < 0 || !format || fstat(t.fd, &st) != 0) return nullptr;
  for (int i = 0; i < n; i++) {
    if (table[i].id && table[i].ino == st.st_ino && table[i].format == format) {
      return &table[i];
    }
  }
  Imported* b = nullptr;
  for (int i = 0; i < n && !b; i++) {
    if (!table[i].id) b = &table[i];
  }
  for (int i = 0; i < n && !b; i++) {
    bool held = table[i].busy;
    for (int k = 0; k < kept; k++) held = held || keep[k] == &table[i];
    if (held) continue;
    lxb_msg m = message(kBufferRemove);
    lxb_put_u32(&m, table[i].id);
    lxb_send(g.sock, &m, -1);
    b = &table[i];
  }
  if (!b) return nullptr;
  *b = {st.st_ino, format, g.nextBuffer++, false};
  lxb_msg m = message(kBufferAdd);
  const uint32_t fields[] = {
    b->id,
    kDisplay,
    uint32_t(t.width),
    uint32_t(t.height),
    format,
    uint32_t(t.stride),
    0
  };
  for (uint32_t v : fields) lxb_put_u32(&m, v);
  lxb_put_u64(&m, 0);  // LINEAR
  if (lxb_send(g.sock, &m, t.fd) != 0) {
    *b = {};
    return nullptr;
  }
  return b;
}

// Shows `s` in the window. Called with g.mu held.
void present(const Shown& s) {
  if (g.sock < 0) return;
  Imported* frame = import(
    g.frames, kMaxFrames, s.frame, drmFormat(s.frame.format, s.below > 0),
    nullptr, 0
  );
  if (!frame) return;
  // render_service commits its last frame again when only a video layer
  // changed, and that frame stays on screen.
  if (frame->id != g.sentFrame || !frame->busy) {
    // render_service may draw into a frame as soon as Commit returns, so the
    // compositor must be done with it.
    if (!waitIdle(frame, 2 * kPeriodNs)) return;
    lxb_msg m = message(kFrame);
    const uint32_t fields[] = {kDisplay, frame->id, 0, 0, 0, 0};
    for (uint32_t v : fields) lxb_put_u32(&m, v);
    if (lxb_send(g.sock, &m, -1) != 0) return;
    frame->busy = true;
    g.sentFrame = frame->id;
  }
  Imported* video[kMaxDeviceLayers];
  SentLayers sent;
  for (int i = 0; i < s.count; i++) {
    Imported* b =
      import(g.videos, kMaxVideoBuffers, s.layer[i], kNv12, video, sent.count);
    if (!b) continue;
    video[sent.count] = b;
    sent.id[sent.count] = b->id;
    sent.region[sent.count] = s.region[i];
    sent.crop[sent.count] = s.crop[i];
    sent.count++;
    if (i < s.below) sent.below++;
  }
  bool changed =
    sent.count != g.sentLayers.count || sent.below != g.sentLayers.below;
  for (int i = 0; i < sent.count && !changed; i++) {
    changed = sent.id[i] != g.sentLayers.id[i] ||
              !same(sent.region[i], g.sentLayers.region[i]) ||
              !same(sent.crop[i], g.sentLayers.crop[i]);
  }
  if (changed) {
    lxb_msg m = message(kLayers);
    lxb_put_u32(&m, kDisplay);
    lxb_put_u32(&m, uint32_t(sent.below));
    lxb_put_u32(&m, uint32_t(sent.count));
    for (int i = 0; i < sent.count; i++) {
      const IRect& r = sent.region[i];
      const IRect& c = sent.crop[i];
      const int32_t fields[] = {
        int32_t(sent.id[i]), r.x, r.y, r.w, r.h, c.x, c.y, c.w, c.h
      };
      for (int32_t v : fields) lxb_put_u32(&m, uint32_t(v));
    }
    if (lxb_send(g.sock, &m, -1) == 0) {
      for (int i = 0; i < sent.count; i++) video[i]->busy = true;
      g.sentLayers = sent;
    }
  }
  // render_service draws into the other frames, and the decoder into the
  // other video buffers, once Commit returns.
  for (auto& i : g.frames) {
    if (&i != frame) waitIdle(&i, 2 * kPeriodNs);
  }
  for (auto& i : g.videos) {
    bool shown = false;
    for (int k = 0; k < sent.count; k++) shown = shown || video[k] == &i;
    if (!shown) waitIdle(&i, 2 * kPeriodNs);
  }
}

// Sends the visible part of cursor image `px` (RGBA, kCursorSize square,
// with its top left at display pixel `(left, top)`) to the bridge, or hides the
// cursor when it has none. The host compositor draws it at the host pointer.
// Called with g.mu held.
void sendCursor(const uint32_t* px, int32_t left, int32_t top) {
  int x0 = kCursorSize, y0 = kCursorSize, x1 = -1, y1 = -1;
  for (int y = 0; y < kCursorSize; y++) {
    for (int x = 0; x < kCursorSize; x++) {
      if (!(px[y * kCursorSize + x] >> 24)) continue;
      x0 = x < x0 ? x : x0;
      x1 = x > x1 ? x : x1;
      y0 = y < y0 ? y : y0;
      y1 = y > y1 ? y : y1;
    }
  }
  lxb_msg m = message(kCursor);
  if (x1 < 0) {
    const uint32_t fields[] = {kDisplay, 0, 0, 0, 0};
    for (uint32_t v : fields) lxb_put_u32(&m, v);
    g.cursorSent = lxb_send(g.sock, &m, -1) == 0;
    return;
  }
  const int w = x1 - x0 + 1, h = y1 - y0 + 1;
  const int fd = memfd_create("cursor", MFD_CLOEXEC | MFD_ALLOW_SEALING);
  if (fd < 0) return;
  // OH's RGBA bytes become wl_shm's BGRA.
  uint32_t row[kCursorSize];
  bool ok = true;
  for (int y = y0; y <= y1 && ok; y++) {
    for (int x = x0; x <= x1; x++) {
      const uint32_t p = px[y * kCursorSize + x];
      row[x - x0] = (p & 0xff00ff00) | (p & 0xff) << 16 | (p >> 16 & 0xff);
    }
    ok = write(fd, row, w * 4) == w * 4;
  }
  if (
    ok &&
    fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE) == 0
  ) {
    const int32_t fields[] = {
      int32_t(kDisplay),   w, h, left + x0, top + y0, kCursorSize / 2 - x0,
      kCursorSize / 2 - y0
    };
    for (int32_t v : fields) lxb_put_u32(&m, uint32_t(v));
    g.cursorSent = lxb_send(g.sock, &m, fd) == 0;
  }
  close(fd);
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
    const timespec wake = at(next);
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &wake, nullptr) ==
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

void closeFd(int& fd) {
  if (fd >= 0) close(fd);
  fd = -1;
}

int dupFd(int fd) { return fd >= 0 ? fcntl(fd, F_DUPFD_CLOEXEC, 0) : -1; }

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
    startThreads();
  }
  const Mode d = mode();
  LOGI("display %u: %ux%u at %u dpi", kDisplay, d.width, d.height, d.dpi);
  if (cb) cb(kDisplay, true, data);
  return HDF_SUCCESS;
}

EXPORT int32_t GetDisplayCapability(uint32_t devId, DisplayCapability& info) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  setString(info.name, "innkeeper");
  info.type = DISP_INTF_BUILT_IN;
  // DMS derives the x and y dpi that apps see from the physical size. MMI
  // scales mouse motion by it, and the launcher writes MMI a curve that
  // keeps one unit of motion at one pixel for this size.
  const Mode d = mode();
  info.phyWidth = d.mmWidth;
  info.phyHeight = d.mmHeight;
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

// MMI draws its cursor into buffers of its own and gives them to
// UpdateHardwareCursor, and render_service leaves the cursor out of the frame.
EXPORT int32_t
GetDisplayProperty(uint32_t devId, uint32_t id, uint64_t& value) {
  if (devId != kDisplay || id != DISPLAY_CAPBILITY_HARDWARE_CURSOR) {
    return HDF_ERR_NOT_SUPPORT;
  }
  value = 1;
  return HDF_SUCCESS;
}

// MMI calls this on each move with the cursor's position and image. The
// bridge shows the image as the host's cursor, so only a new image matters.
EXPORT int32_t UpdateHardwareCursor(
  uint32_t devId, int32_t x, int32_t y, BufferHandle* buffer
) {
  if (devId != kDisplay || !buffer) return HDF_ERR_INVALID_PARAM;
  const BufferHandle& b = *buffer;
  if (
    b.width != kCursorSize || b.height != kCursorSize ||
    b.stride < kCursorSize * 4 ||
    drmFormat(b.format) != drmFormat(PIXEL_FMT_RGBA_8888)
  ) {
    return HDF_ERR_INVALID_PARAM;
  }
  const size_t size = size_t(b.stride) * b.height;
  void* map = mmap(nullptr, size, PROT_READ, MAP_SHARED, b.fd, 0);
  if (map == MAP_FAILED) return HDF_FAILURE;
  dma_buf_sync sync = {DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ};
  ioctl(b.fd, DMA_BUF_IOCTL_SYNC, &sync);
  {
    Lock l;
    bool changed = false;
    for (int y = 0; y < kCursorSize; y++) {
      const void* row = static_cast<const char*>(map) + size_t(y) * b.stride;
      uint32_t* to = &g.cursor[y * kCursorSize];
      if (memcmp(to, row, kCursorSize * 4) == 0) continue;
      memcpy(to, row, kCursorSize * 4);
      changed = true;
    }
    if (g.sock >= 0 && (changed || !g.cursorSent)) sendCursor(g.cursor, x, y);
  }
  sync.flags = DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ;
  ioctl(b.fd, DMA_BUF_IOCTL_SYNC, &sync);
  munmap(map, size);
  return HDF_SUCCESS;
}

EXPORT int32_t SetDisplayProperty(uint32_t, uint32_t, uint64_t) {
  return HDF_ERR_NOT_SUPPORT;
}

// When the composer shows render_service's uni-render output as the frame,
// render_service does not copy it into a client target with GL on its
// composer thread. Video layers that the bridge shows, render_service does
// not draw at all.
EXPORT int32_t PrepareDisplayLayers(uint32_t devId, bool& needFlushFb) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  Lock l;
  static const Mode d = mode();
  const IRect screen = {0, 0, int32_t(d.width), int32_t(d.height)};
  const Choice c = choose(g.layers, kMaxLayers, screen);
  if (c.frame != g.choice.frame || c.count != g.choice.count) {
    LOGI(
      "composing %s with %d video layers",
      c.frame >= 0 ? "a device layer" : "a client target", c.count
    );
  }
  g.choice = c;
  for (int i = 0; i < kMaxLayers; i++) {
    Layer& x = g.layers[i];
    bool shown = i == c.frame;
    for (int k = 0; k < c.count; k++) shown = shown || c.layers[k] == i;
    if (x.used && !x.cursor && !shown && x.type != COMPOSITION_CLIENT) {
      x.changed = true;
    }
  }
  needFlushFb = c.frame < 0;
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
  closeFd(g.client.fd);
  closeFd(g.clientFence);
  g.client = {
    dupFd(buffer.fd), buffer.width, buffer.height, buffer.stride, buffer.format
  };
  g.clientFence = dupFd(fence);
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

// The composer shows the last frame again in a new window, so it needs no
// refresh from render_service.
EXPORT int32_t RegRefreshCallback(RefreshCallback, void*) {
  return HDF_SUCCESS;
}

// render_service's screen manager gives up unless these register. We have no
// HWC events or idle vblanks to report.
EXPORT int32_t RegHwcEventCallback(void*, void*) { return HDF_SUCCESS; }
EXPORT int32_t RegDisplayVBlankIdleCallback(void*, void*) {
  return HDF_SUCCESS;
}

// There are no release fences, because Commit returns only once the
// compositor is done with every target except the new one.
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
  int acquire[1 + kMaxDeviceLayers];
  int n = 0;
  pthread_mutex_lock(&g.mu);
  const Choice c = g.choice;
  int& pending = c.frame >= 0 ? g.layers[c.frame].fence : g.clientFence;
  acquire[n++] = pending;
  pending = -1;
  for (int i = 0; i < c.count; i++) {
    acquire[n++] = g.layers[c.layers[i]].fence;
    g.layers[c.layers[i]].fence = -1;
  }
  pthread_mutex_unlock(&g.mu);
  for (int i = 0; i < n; i++) {
    if (acquire[i] < 0) continue;
    pollfd p = {acquire[i], POLLIN, 0};
    if (poll(&p, 1, 1000) != 1) LOGW("acquire fence timeout");
    close(acquire[i]);
  }
  Lock l;
  Shown& s = g.shown;
  closeFd(s.frame.fd);
  for (int i = 0; i < s.count; i++) closeFd(s.layer[i].fd);
  s.frame = c.frame >= 0 ? g.layers[c.frame].buffer : g.client;
  s.frame.fd = dupFd(s.frame.fd);
  s.count = 0;
  s.below = 0;
  for (int i = 0; i < c.count; i++) {
    const Layer& x = g.layers[c.layers[i]];
    if (!x.used) continue;
    s.layer[s.count] = x.buffer;
    s.layer[s.count].fd = dupFd(x.buffer.fd);
    s.region[s.count] = c.region[i];
    s.crop[s.count] = c.crop[i];
    s.count++;
    if (i < c.below) s.below++;
  }
  present(s);
  return HDF_SUCCESS;
}

// --- layers ------------------------------------------------------------------

EXPORT int32_t
CreateLayer(uint32_t devId, const LayerInfo& info, uint32_t& layerId) {
  if (devId != kDisplay) return HDF_ERR_INVALID_PARAM;
  Lock l;
  for (uint32_t i = 0; i < kMaxLayers; i++) {
    if (g.layers[i].used) continue;
    g.layers[i] = Layer{};
    g.layers[i].used = true;
    g.layers[i].cursor = info.type == LAYER_TYPE_CURSOR;
    layerId = i;
    return HDF_SUCCESS;
  }
  return HDF_FAILURE;
}

EXPORT int32_t DestroyLayer(uint32_t devId, uint32_t layerId) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  closeFd(x->buffer.fd);
  closeFd(x->fence);
  *x = {};
  // Commit shows what remains of the last choice.
  if (g.choice.frame == int(layerId)) g.choice = {};
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

EXPORT int32_t SetLayerBuffer(
  uint32_t devId, uint32_t layerId, const BufferHandle& buffer, int32_t fence
) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  closeFd(x->buffer.fd);
  closeFd(x->fence);
  x->buffer = {
    dupFd(buffer.fd), buffer.width, buffer.height, buffer.stride, buffer.format
  };
  x->fence = dupFd(fence);
  return HDF_SUCCESS;
}

EXPORT int32_t
SetLayerRegion(uint32_t devId, uint32_t layerId, const IRect& rect) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  x->region = rect;
  return HDF_SUCCESS;
}

EXPORT int32_t
SetLayerCrop(uint32_t devId, uint32_t layerId, const IRect& rect) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  x->crop = rect;
  return HDF_SUCCESS;
}

EXPORT int32_t
SetLayerTransformMode(uint32_t devId, uint32_t layerId, int32_t type) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  x->transform = type;
  return HDF_SUCCESS;
}

EXPORT int32_t SetLayerZorder(uint32_t devId, uint32_t layerId, uint32_t z) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  x->z = z;
  return HDF_SUCCESS;
}

EXPORT int32_t
SetLayerAlpha(uint32_t devId, uint32_t layerId, const LayerAlpha& alpha) {
  Lock l;
  Layer* x = layer(devId, layerId);
  if (!x) return HDF_ERR_INVALID_PARAM;
  x->translucent = alpha.enGlobalAlpha && alpha.gAlpha < 255;
  return HDF_SUCCESS;
}

// The composer shows a frame that fills the screen and opaque video layers
// (layers.h). The frame's own pixels say where it is transparent, and
// render_service premultiplies them, as Wayland expects. The rest of these
// settings don't change what the composer shows.
#define LAYER_SETTER(name, ...)                                         \
  EXPORT int32_t name(uint32_t devId, uint32_t layerId, __VA_ARGS__) {  \
    Lock l;                                                             \
    return layer(devId, layerId) ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM; \
  }
LAYER_SETTER(SetLayerPreMulti, bool)
LAYER_SETTER(SetLayerDirtyRegion, const lxvec<IRect>&)
LAYER_SETTER(SetLayerVisibleRegion, lxvec<IRect>&)
LAYER_SETTER(SetLayerBlendType, int32_t)
LAYER_SETTER(SetLayerMaskInfo, int32_t)
LAYER_SETTER(SetLayerColor, const void*)
