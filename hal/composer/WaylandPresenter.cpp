// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <algorithm>
#include <atomic>
#include <deque>
#include <map>
#include <mutex>
#include <optional>
#include <set>
#include <thread>
#include <vector>

#include <errno.h>
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <poll.h>
#include <pthread.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <wayland-client.h>

#include "WaylandPresenter.h"

#include "InputServer.h"
#include "Properties.h"
#include "fractional-scale-v1-client-protocol.h"
#include "linux-dmabuf-v1-client-protocol.h"
#include "linuxapk/layout.h"
#include "linuxapk/log.h"
#include "presentation-time-client-protocol.h"
#include "viewporter-client-protocol.h"
#include "xdg-activation-v1-client-protocol.h"
#include "xdg-decoration-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

namespace linuxapk::hwc {

namespace {
constexpr uint32_t kDrmXBGR8888 =
  fourcc('X', 'B', '2', '4');  // R,G,B,X bytes = RGBA_8888
constexpr uint64_t kModLinear = 0;
constexpr int kPoolSize = 3;
// A window_new request that no virtual display claimed by then is dropped.
constexpr int64_t kPendingTimeoutNs = 10'000'000'000;
constexpr int32_t kBaseDpi = 160;

template <typename T>
T* bindGlobal(
  wl_registry* r, uint32_t name, const wl_interface* iface, uint32_t have,
  uint32_t want
) {
  return static_cast<T*>(
    wl_registry_bind(r, name, iface, std::min(have, want))
  );
}

std::string sanitize(std::string s) {
  for (char& ch : s) {
    if (ch == '\n' || ch == '\r') ch = ' ';
  }
  return s;
}
}  // namespace

// What a window shows and how it is labelled.
struct WindowSpec {
  int32_t id = 0;  // Android display id (the key in input events)
  uint32_t width = 0, height = 0;  // display pixels
  int32_t dpi = kBaseDpi;
  int32_t minW = 0, minH = 0;  // logical; 0 = our default minimum
  std::string appId, title;
};

struct PoolBuffer {
  int memfd = -1;
  int dmabuf = -1;
  uint8_t* map = nullptr;
  size_t size = 0;
  wl_buffer* buffer = nullptr;
  bool busy = false;  // attached and not yet released by the compositor
};

// Buffers of one size. A window replaces its pool when the display resizes.
struct Pool {
  std::vector<PoolBuffer> bufs;
  uint32_t width = 0, height = 0, stride = 0;
  bool dmabuf = false;
  bool bgrx = false;  // pixels are B,G,R,X (else R,G,B,X)
  void destroy();
};

void Pool::destroy() {
  for (auto& b : bufs) {
    if (b.buffer) wl_buffer_destroy(b.buffer);
    if (b.map) munmap(b.map, b.size);
    if (b.dmabuf >= 0) close(b.dmabuf);
    if (b.memfd >= 0) close(b.memfd);
  }
  bufs.clear();
}

struct WaylandConnection::Impl {
  wl_display* display = nullptr;
  wl_registry* registry = nullptr;
  wl_compositor* compositor = nullptr;
  wl_shm* shm = nullptr;
  xdg_wm_base* wmBase = nullptr;
  wp_viewporter* viewporter = nullptr;
  zwp_linux_dmabuf_v1* dmabuf = nullptr;
  wp_fractional_scale_manager_v1* fractional = nullptr;
  zxdg_decoration_manager_v1* decorations = nullptr;
  wp_presentation* presentation = nullptr;
  xdg_activation_v1* activation = nullptr;
  wl_seat* seat = nullptr;
  wl_pointer* pointer = nullptr;
  wl_keyboard* keyboard = nullptr;
  wl_touch* touch = nullptr;

  std::set<uint32_t> linearDmabufFormats;
  std::set<uint32_t> shmFormats;

  // Guards everything below and every window's state; the event thread's
  // listeners take it.
  std::mutex mutex;
  std::map<int32_t, std::shared_ptr<WaylandWindow>> windows;
  struct Pending {
    WindowSpec spec;
    int64_t time;
  };
  std::deque<Pending> pending;  // window_new requests, oldest first
  int32_t pointerFocus = -1, keyboardFocus = -1;  // window ids, -1 = none
  std::map<int32_t, int32_t> touchFocus;          // host touch id -> window id
  uint32_t lastSerial = 0;  // newest input serial, for xdg_activation
  InputServer input;

  int wakeFd = -1;
  std::thread thread;
  std::atomic<bool> stop{false};

  ~Impl();
  void loop();
  // Surface-local logical -> display pixels of window `id`.
  void toDisplay(
    int32_t id, wl_fixed_t sx, wl_fixed_t sy, double* x, double* y
  );
  int32_t windowIdOf(wl_surface* s);  // requires mutex; -1 if not ours

  // Window registry.
  void onCommand(const std::string& line);
  std::optional<WindowSpec> claimPending(uint32_t w, uint32_t h);
  bool addWindow(std::shared_ptr<WaylandWindow> w);
  std::shared_ptr<WaylandWindow> findWindow(int32_t id, int64_t hwcId);
  // Removes window `id` if it shows `hwcId` (-1: any) and announces it.
  void removeWindow(int32_t id, int64_t hwcId);
};

class WaylandWindow {
 public:
  WaylandWindow(WaylandConnection::Impl* c, WindowSpec spec, int64_t hwcId);
  ~WaylandWindow();
  bool init();
  Error present(FrameInput& frame);
  int32_t id() const { return mSpec.id; }
  int64_t hwcId() const { return mHwcId; }
  void setTitle(const std::string& title);
  void setMinSize(int32_t w, int32_t h);
  void activate();

  // Called with c->mutex held.
  void onConfigure(int32_t w, int32_t h);
  void onSurfaceConfigure(uint32_t serial);
  void onRelease(wl_buffer* b);
  void onScale(uint32_t scale120);
  void onPresented(bool presented, uint32_t refreshNs);
  void onActivationToken(const char* token);
  void mapToDisplay(double sx, double sy, double* x, double* y) const;
  wl_surface* surface() const { return mSurface; }

 private:
  bool createPool(uint32_t w, uint32_t h, bool dmabuf, Pool* out);
  bool wantDmabuf() const;
  void commitLocked(int index);
  void updateViewportLocked();

  struct Ctx* mCtx = nullptr;
  WaylandConnection::Impl* c;
  WindowSpec mSpec;
  const int64_t mHwcId;  // -1 for display 0
  wl_surface* mSurface = nullptr;
  xdg_surface* mXdgSurface = nullptr;
  xdg_toplevel* mToplevel = nullptr;
  wp_viewport* mViewport = nullptr;
  wp_fractional_scale_v1* mFractional = nullptr;
  zxdg_toplevel_decoration_v1* mDecoration = nullptr;
  Pool mPool;
  std::vector<Pool> mRetired;  // replaced pools, freed after the next commit
  bool mConfigured = false;
  int32_t mPendingW = 0, mPendingH = 0;
  int32_t mWinW, mWinH;  // logical window size == viewport destination
  double mScale = 1.0;
  int mLast = -1;  // pool index of the newest frame
  uint64_t mFrames = 0, mDropped = 0, mPresented = 0, mDiscarded = 0;
  uint32_t mRefreshNs = 0;
  int64_t mLastLogNs = 0;
  bool mWarnedFormat = false;
};

// Listener data for a window's Wayland objects. It outlives the window
// (events may still be queued for its objects), so the window clears `w`.
struct Ctx {
  WaylandConnection::Impl* c;
  WaylandWindow* w;
};

// ---- listeners -------------------------------------------------------------

static void onRegistryGlobal(
  void* data, wl_registry* r, uint32_t name, const char* iface, uint32_t version
) {
  auto* c = static_cast<WaylandConnection::Impl*>(data);
  std::string i = iface;
  if (i == wl_compositor_interface.name) {
    c->compositor =
      bindGlobal<wl_compositor>(r, name, &wl_compositor_interface, version, 5);
  } else if (i == wl_shm_interface.name) {
    c->shm = bindGlobal<wl_shm>(r, name, &wl_shm_interface, version, 1);
  } else if (i == xdg_wm_base_interface.name) {
    c->wmBase =
      bindGlobal<xdg_wm_base>(r, name, &xdg_wm_base_interface, version, 5);
  } else if (i == wp_viewporter_interface.name) {
    c->viewporter =
      bindGlobal<wp_viewporter>(r, name, &wp_viewporter_interface, version, 1);
  } else if (i == zwp_linux_dmabuf_v1_interface.name && version >= 3) {
    // v3 announces (format, modifier) pairs directly; enough for LINEAR.
    c->dmabuf = bindGlobal<zwp_linux_dmabuf_v1>(
      r, name, &zwp_linux_dmabuf_v1_interface, version, 3
    );
  } else if (i == wp_fractional_scale_manager_v1_interface.name) {
    c->fractional = bindGlobal<wp_fractional_scale_manager_v1>(
      r, name, &wp_fractional_scale_manager_v1_interface, version, 1
    );
  } else if (i == zxdg_decoration_manager_v1_interface.name) {
    c->decorations = bindGlobal<zxdg_decoration_manager_v1>(
      r, name, &zxdg_decoration_manager_v1_interface, version, 1
    );
  } else if (i == wp_presentation_interface.name) {
    c->presentation = bindGlobal<wp_presentation>(
      r, name, &wp_presentation_interface, version, 1
    );
  } else if (i == xdg_activation_v1_interface.name) {
    c->activation = bindGlobal<xdg_activation_v1>(
      r, name, &xdg_activation_v1_interface, version, 1
    );
  } else if (i == wl_seat_interface.name && !c->seat) {
    c->seat = bindGlobal<wl_seat>(r, name, &wl_seat_interface, version, 8);
  }
}
static void onRegistryRemove(void*, wl_registry*, uint32_t) {}
static const wl_registry_listener kRegistryListener = {
  onRegistryGlobal, onRegistryRemove
};

static void onShmFormat(void* data, wl_shm*, uint32_t format) {
  static_cast<WaylandConnection::Impl*>(data)->shmFormats.insert(format);
}
static const wl_shm_listener kShmListener = {onShmFormat};

static void onDmabufFormat(void*, zwp_linux_dmabuf_v1*, uint32_t) {}
static void onDmabufModifier(
  void* data, zwp_linux_dmabuf_v1*, uint32_t format, uint32_t hi, uint32_t lo
) {
  if ((uint64_t(hi) << 32 | lo) == kModLinear) {
    static_cast<WaylandConnection::Impl*>(data)->linearDmabufFormats.insert(
      format
    );
  }
}
static const zwp_linux_dmabuf_v1_listener kDmabufListener = {
  onDmabufFormat, onDmabufModifier
};

static void onWmPing(void*, xdg_wm_base* b, uint32_t serial) {
  xdg_wm_base_pong(b, serial);
}
static const xdg_wm_base_listener kWmBaseListener = {onWmPing};

// Pointer, keyboard, touch -> InputServer. Enter events say which window
// (Android display) the following events belong to (protocol v2).
#define C static_cast<WaylandConnection::Impl*>(data)
static void ptrEnter(
  void* data, wl_pointer*, uint32_t serial, wl_surface* s, wl_fixed_t sx,
  wl_fixed_t sy
) {
  int32_t id;
  {
    std::lock_guard lock(C->mutex);
    id = C->windowIdOf(s);
    C->pointerFocus = id;
    C->lastSerial = serial;
  }
  if (id < 0) return;
  double x, y;
  C->toDisplay(id, sx, sy, &x, &y);
  C->input.send("pointer_enter %.2f %.2f %d", x, y, id);
}
static void ptrLeave(void* data, wl_pointer*, uint32_t, wl_surface*) {
  int32_t id;
  {
    std::lock_guard lock(C->mutex);
    id = C->pointerFocus;
    C->pointerFocus = -1;
  }
  if (id >= 0) C->input.send("pointer_leave %d", id);
}
static void ptrMotion(
  void* data, wl_pointer*, uint32_t t, wl_fixed_t sx, wl_fixed_t sy
) {
  int32_t id;
  {
    std::lock_guard lock(C->mutex);
    id = C->pointerFocus;
  }
  if (id < 0) return;
  double x, y;
  C->toDisplay(id, sx, sy, &x, &y);
  C->input.send("pointer_motion %u %.2f %.2f", t, x, y);
}
static void ptrButton(
  void* data, wl_pointer*, uint32_t serial, uint32_t t, uint32_t button,
  uint32_t state
) {
  {
    std::lock_guard lock(C->mutex);
    C->lastSerial = serial;
  }
  C->input.send("pointer_button %u %u %u", t, button, state);
}
static void ptrAxis(
  void* data, wl_pointer*, uint32_t t, uint32_t axis, wl_fixed_t value
) {
  C->input.send("pointer_axis %u %u %.3f", t, axis, wl_fixed_to_double(value));
}
static void ptrFrame(void* data, wl_pointer*) {
  C->input.send("pointer_frame");
}
static void ptrAxisSource(void*, wl_pointer*, uint32_t) {}
static void ptrAxisStop(void* data, wl_pointer*, uint32_t t, uint32_t axis) {
  C->input.send("pointer_axis_stop %u %u", t, axis);
}
static void ptrAxisDiscrete(void*, wl_pointer*, uint32_t, int32_t) {}
static void ptrAxisValue120(
  void* data, wl_pointer*, uint32_t axis, int32_t v120
) {
  C->input.send("pointer_axis_value120 %u %d", axis, v120);
}
static void ptrAxisDirection(void*, wl_pointer*, uint32_t, uint32_t) {}
static const wl_pointer_listener kPointerListener = {
  ptrEnter,        ptrLeave,        ptrMotion,       ptrButton,
  ptrAxis,         ptrFrame,        ptrAxisSource,   ptrAxisStop,
  ptrAxisDiscrete, ptrAxisValue120, ptrAxisDirection
};

static void kbKeymap(void*, wl_keyboard*, uint32_t, int32_t fd, uint32_t) {
  close(fd);
}
static void kbEnter(
  void* data, wl_keyboard*, uint32_t serial, wl_surface* s, wl_array*
) {
  int32_t id;
  {
    std::lock_guard lock(C->mutex);
    id = C->windowIdOf(s);
    C->keyboardFocus = id;
    C->lastSerial = serial;
  }
  if (id >= 0) C->input.send("keyboard_enter %d", id);
}
static void kbLeave(void* data, wl_keyboard*, uint32_t, wl_surface*) {
  int32_t id;
  {
    std::lock_guard lock(C->mutex);
    id = C->keyboardFocus;
    C->keyboardFocus = -1;
  }
  if (id >= 0) C->input.send("keyboard_leave %d", id);
}
static void kbKey(
  void* data, wl_keyboard*, uint32_t serial, uint32_t t, uint32_t key,
  uint32_t state
) {
  {
    std::lock_guard lock(C->mutex);
    C->lastSerial = serial;
    if (C->keyboardFocus < 0) return;
  }
  C->input.send("key %u %u %u", t, key, state);
}
static void kbModifiers(
  void* data, wl_keyboard*, uint32_t, uint32_t dep, uint32_t lat, uint32_t lock,
  uint32_t group
) {
  C->input.send("modifiers %u %u %u %u", dep, lat, lock, group);
}
static void kbRepeat(void*, wl_keyboard*, int32_t, int32_t) {}
static const wl_keyboard_listener kKeyboardListener = {
  kbKeymap, kbEnter, kbLeave, kbKey, kbModifiers, kbRepeat
};

static void tDown(
  void* data, wl_touch*, uint32_t serial, uint32_t t, wl_surface* s,
  int32_t tid, wl_fixed_t sx, wl_fixed_t sy
) {
  int32_t id;
  {
    std::lock_guard lock(C->mutex);
    id = C->windowIdOf(s);
    if (id >= 0) C->touchFocus[tid] = id;
    C->lastSerial = serial;
  }
  if (id < 0) return;
  double x, y;
  C->toDisplay(id, sx, sy, &x, &y);
  C->input.send("touch_down %u %d %.2f %.2f %d", t, tid, x, y, id);
}
static void tUp(void* data, wl_touch*, uint32_t, uint32_t t, int32_t tid) {
  {
    std::lock_guard lock(C->mutex);
    if (!C->touchFocus.erase(tid)) return;
  }
  C->input.send("touch_up %u %d", t, tid);
}
static void tMotion(
  void* data, wl_touch*, uint32_t t, int32_t tid, wl_fixed_t sx, wl_fixed_t sy
) {
  int32_t id = -1;
  {
    std::lock_guard lock(C->mutex);
    auto it = C->touchFocus.find(tid);
    if (it != C->touchFocus.end()) id = it->second;
  }
  if (id < 0) return;
  double x, y;
  C->toDisplay(id, sx, sy, &x, &y);
  C->input.send("touch_motion %u %d %.2f %.2f", t, tid, x, y);
}
static void tFrame(void* data, wl_touch*) { C->input.send("touch_frame"); }
static void tCancel(void* data, wl_touch*) {
  {
    std::lock_guard lock(C->mutex);
    C->touchFocus.clear();
  }
  C->input.send("touch_cancel");
}
static void tShape(void*, wl_touch*, int32_t, wl_fixed_t, wl_fixed_t) {}
static void tOrientation(void*, wl_touch*, int32_t, wl_fixed_t) {}
static const wl_touch_listener kTouchListener = {
  tDown, tUp, tMotion, tFrame, tCancel, tShape, tOrientation
};

static void seatCaps(void* data, wl_seat* seat, uint32_t caps) {
  auto* c = C;
  if ((caps & WL_SEAT_CAPABILITY_POINTER) && !c->pointer) {
    c->pointer = wl_seat_get_pointer(seat);
    wl_pointer_add_listener(c->pointer, &kPointerListener, c);
  }
  if ((caps & WL_SEAT_CAPABILITY_KEYBOARD) && !c->keyboard) {
    c->keyboard = wl_seat_get_keyboard(seat);
    wl_keyboard_add_listener(c->keyboard, &kKeyboardListener, c);
  }
  if ((caps & WL_SEAT_CAPABILITY_TOUCH) && !c->touch) {
    c->touch = wl_seat_get_touch(seat);
    wl_touch_add_listener(c->touch, &kTouchListener, c);
  }
}
static void seatName(void*, wl_seat*, const char*) {}
static const wl_seat_listener kSeatListener = {seatCaps, seatName};
#undef C

// Window listeners: data is the window's Ctx; they take the connection lock
// and do nothing once the window is gone.
#define WINDOW_OR_RETURN()             \
  auto* ctx = static_cast<Ctx*>(data); \
  std::lock_guard lock(ctx->c->mutex); \
  if (!ctx->w) return;

static void xdgSurfaceConfigure(void* data, xdg_surface*, uint32_t serial) {
  WINDOW_OR_RETURN();
  ctx->w->onSurfaceConfigure(serial);
}
static const xdg_surface_listener kXdgSurfaceListener = {xdgSurfaceConfigure};

static void toplevelConfigure(
  void* data, xdg_toplevel*, int32_t w, int32_t h, wl_array*
) {
  WINDOW_OR_RETURN();
  ctx->w->onConfigure(w, h);
}
static void toplevelClose(void* data, xdg_toplevel*) {
  auto* ctx = static_cast<Ctx*>(data);
  int32_t id;
  {
    std::lock_guard lock(ctx->c->mutex);
    if (!ctx->w) return;
    id = ctx->w->id();
  }
  ctx->c->input.send("close %d", id);
}
static void toplevelBounds(void*, xdg_toplevel*, int32_t, int32_t) {}
static void toplevelCaps(void*, xdg_toplevel*, wl_array*) {}
static const xdg_toplevel_listener kToplevelListener = {
  toplevelConfigure, toplevelClose, toplevelBounds, toplevelCaps
};

static void bufferRelease(void* data, wl_buffer* b) {
  WINDOW_OR_RETURN();
  ctx->w->onRelease(b);
}
static const wl_buffer_listener kBufferListener = {bufferRelease};

static void preferredScale(
  void* data, wp_fractional_scale_v1*, uint32_t scale120
) {
  WINDOW_OR_RETURN();
  ctx->w->onScale(scale120);
}
static const wp_fractional_scale_v1_listener kFractionalListener = {
  preferredScale
};

static void fbSyncOutput(void*, struct wp_presentation_feedback*, wl_output*) {}
static void fbPresented(
  void* data, struct wp_presentation_feedback* f, uint32_t, uint32_t, uint32_t,
  uint32_t refresh, uint32_t, uint32_t, uint32_t
) {
  wp_presentation_feedback_destroy(f);
  WINDOW_OR_RETURN();
  ctx->w->onPresented(true, refresh);
}
static void fbDiscarded(void* data, struct wp_presentation_feedback* f) {
  wp_presentation_feedback_destroy(f);
  WINDOW_OR_RETURN();
  ctx->w->onPresented(false, 0);
}
static const wp_presentation_feedback_listener kFeedbackListener = {
  fbSyncOutput, fbPresented, fbDiscarded
};

static void tokenDone(
  void* data, xdg_activation_token_v1* t, const char* token
) {
  xdg_activation_token_v1_destroy(t);
  WINDOW_OR_RETURN();
  ctx->w->onActivationToken(token);
}
static const xdg_activation_token_v1_listener kTokenListener = {tokenDone};
#undef WINDOW_OR_RETURN

// ---- connection ------------------------------------------------------------

static WaylandConnection* gConnection = nullptr;

WaylandConnection::WaylandConnection(std::unique_ptr<Impl> impl)
    : mImpl(std::move(impl)) {}
WaylandConnection::~WaylandConnection() = default;
WaylandConnection* WaylandConnection::get() { return gConnection; }

WaylandConnection::Impl::~Impl() {
  stop = true;
  if (thread.joinable()) {
    uint64_t one = 1;
    (void)!write(wakeFd, &one, sizeof one);
    thread.join();
  }
  if (display) wl_display_disconnect(display);
  if (wakeFd >= 0) close(wakeFd);
}

int32_t WaylandConnection::Impl::windowIdOf(wl_surface* s) {
  if (!s) return -1;
  for (auto& [id, w] : windows) {
    if (w->surface() == s) return id;
  }
  return -1;
}

void WaylandConnection::Impl::toDisplay(
  int32_t id, wl_fixed_t sx, wl_fixed_t sy, double* x, double* y
) {
  std::lock_guard lock(mutex);
  double fx = wl_fixed_to_double(sx), fy = wl_fixed_to_double(sy);
  auto it = windows.find(id);
  if (it != windows.end()) {
    it->second->mapToDisplay(fx, fy, x, y);
  } else {
    *x = fx;
    *y = fy;
  }
}

void WaylandConnection::Impl::loop() {
  pthread_setname_np(pthread_self(), "hwc-wayland");
  while (!stop) {
    while (wl_display_prepare_read(display) != 0)
      wl_display_dispatch_pending(display);
    if (wl_display_flush(display) < 0 && errno != EAGAIN) {
      wl_display_cancel_read(display);
      break;
    }
    pollfd fds[2] = {
      {wl_display_get_fd(display), POLLIN, 0}, {wakeFd, POLLIN, 0}
    };
    if (poll(fds, 2, -1) < 0) {
      wl_display_cancel_read(display);
      if (errno == EINTR) continue;
      break;
    }
    if (fds[0].revents & POLLIN) {
      if (wl_display_read_events(display) < 0) break;
    } else {
      wl_display_cancel_read(display);
    }
    if (fds[1].revents) break;
    if (wl_display_dispatch_pending(display) < 0) break;
  }
  if (!stop) {
    LOGE(
      "wayland: connection lost (%s)", strerror(wl_display_get_error(display))
    );
  }
}

// ---- window registry (window manager commands) -----------------------------

// Commands from the window manager (docs/hal-display.md, protocol v2):
//   window_new <display> <width> <height> <dpi> <app_id> <title...>
//   window_min_size <display> <logical width> <logical height>
//   window_title <display> <title...>
//   window_activate <display>
//   window_destroy <display>
//   ping <token>
void WaylandConnection::Impl::onCommand(const std::string& line) {
  char cmd[32] = {};
  int32_t id = -1;
  int n = 0;
  if (sscanf(line.c_str(), "%31s %d%n", cmd, &id, &n) < 1) return;
  std::string op = cmd;
  if (op == "ping") {
    input.send(
      "pong %s", line.size() > 5 ? sanitize(line.substr(5)).c_str() : ""
    );
    return;
  }
  if (id < 0) {
    LOGW("wm: bad command: %s", line.c_str());
    return;
  }
  const char* rest = line.c_str() + n;
  while (*rest == ' ') rest++;
  if (op == "window_new") {
    WindowSpec spec;
    spec.id = id;
    char appId[256] = {};
    int m = 0;
    if (
      sscanf(
        rest, "%u %u %d %255s%n", &spec.width, &spec.height, &spec.dpi, appId,
        &m
      ) < 4 ||
      spec.width < 1 || spec.height < 1 || spec.dpi < 1
    ) {
      input.send("window_failed %d bad_request", id);
      return;
    }
    spec.appId = appId;
    const char* title = rest + m;
    while (*title == ' ') title++;
    spec.title = sanitize(*title ? title : appId);
    {
      std::lock_guard lock(mutex);
      if (windows.count(id)) {
        input.send("window_failed %d exists", id);
        return;
      }
      const int64_t now = nowNs();
      while (!pending.empty() &&
             now - pending.front().time > kPendingTimeoutNs) {
        input.send("window_failed %d timeout", pending.front().spec.id);
        pending.pop_front();
      }
      std::erase_if(pending, [&](auto& p) { return p.spec.id == id; });
      pending.push_back({spec, now});
    }
    LOGI(
      "wm: window %d requested: %ux%u @ %d dpi, app_id %s, \"%s\"", id,
      spec.width, spec.height, spec.dpi, spec.appId.c_str(), spec.title.c_str()
    );
    input.send("window_pending %d", id);
  } else if (op == "window_min_size") {
    int32_t w = 0, h = 0;
    if (
      sscanf(rest, "%d %d", &w, &h) != 2 || w < 0 || h < 0 || w > 16384 ||
      h > 16384
    )
      return;
    if (std::shared_ptr<WaylandWindow> win = findWindow(id, -1)) {
      win->setMinSize(w, h);
      return;
    }
    std::lock_guard lock(mutex);
    for (auto& p : pending) {
      if (p.spec.id == id) {
        p.spec.minW = w;
        p.spec.minH = h;
      }
    }
  } else if (op == "window_title") {
    std::shared_ptr<WaylandWindow> w = findWindow(id, -1);
    if (w) w->setTitle(sanitize(rest));
  } else if (op == "window_activate") {
    std::shared_ptr<WaylandWindow> w = findWindow(id, -1);
    if (w) w->activate();
  } else if (op == "window_destroy") {
    {
      std::lock_guard lock(mutex);
      std::erase_if(pending, [&](auto& p) { return p.spec.id == id; });
    }
    removeWindow(id, -1);
  } else {
    LOGW("wm: unknown command: %s", line.c_str());
  }
}

std::optional<WindowSpec> WaylandConnection::Impl::claimPending(
  uint32_t w, uint32_t h
) {
  std::lock_guard lock(mutex);
  const int64_t now = nowNs();
  for (auto it = pending.begin(); it != pending.end(); ++it) {
    if (now - it->time > kPendingTimeoutNs) continue;
    if (it->spec.width == w && it->spec.height == h) {
      WindowSpec s = it->spec;
      pending.erase(it);
      return s;
    }
  }
  return std::nullopt;
}

bool WaylandConnection::Impl::addWindow(std::shared_ptr<WaylandWindow> w) {
  std::shared_ptr<WaylandWindow> old;
  {
    std::lock_guard lock(mutex);
    auto& slot = windows[w->id()];
    old = std::move(slot);
    slot = std::move(w);
  }
  return !old;  // `old` (a stale window for the same id) closes here
}

std::shared_ptr<WaylandWindow> WaylandConnection::Impl::findWindow(
  int32_t id, int64_t hwcId
) {
  std::lock_guard lock(mutex);
  auto it = windows.find(id);
  if (it == windows.end()) return nullptr;
  if (hwcId != -1 && it->second->hwcId() != hwcId) return nullptr;
  return it->second;
}

void WaylandConnection::Impl::removeWindow(int32_t id, int64_t hwcId) {
  std::shared_ptr<WaylandWindow> gone;
  {
    std::lock_guard lock(mutex);
    auto it = windows.find(id);
    if (it == windows.end()) return;
    if (hwcId != -1 && it->second->hwcId() != hwcId) return;
    gone = std::move(it->second);
    windows.erase(it);
    if (pointerFocus == id) pointerFocus = -1;
    if (keyboardFocus == id) keyboardFocus = -1;
    std::erase_if(touchFocus, [&](auto& t) { return t.second == id; });
  }
  input.removeWindow(id);
  gone.reset();  // destroys the toplevel (takes the lock itself)
  LOGI("wm: window %d closed", id);
  input.send("window_gone %d", id);
}

bool WaylandConnection::init(
  const std::string& socketPath, const std::string& inputPath
) {
  int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  sockaddr_un addr{};
  addr.sun_family = AF_UNIX;
  if (socketPath.size() >= sizeof(addr.sun_path)) return false;
  memcpy(addr.sun_path, socketPath.c_str(), socketPath.size() + 1);
  if (connect(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0) {
    LOGE("wayland: connect %s: %s", socketPath.c_str(), strerror(errno));
    close(fd);
    return false;
  }
  auto c = std::make_unique<Impl>();
  c->display = wl_display_connect_to_fd(fd);
  if (!c->display) {
    LOGE("wayland: wl_display_connect_to_fd failed");
    close(fd);
    return false;
  }
  c->registry = wl_display_get_registry(c->display);
  wl_registry_add_listener(c->registry, &kRegistryListener, c.get());
  wl_display_roundtrip(c->display);
  if (!c->compositor || !c->wmBase || !c->shm) {
    LOGE("wayland: compositor lacks wl_compositor/xdg_wm_base/wl_shm");
    return false;
  }
  wl_shm_add_listener(c->shm, &kShmListener, c.get());
  xdg_wm_base_add_listener(c->wmBase, &kWmBaseListener, nullptr);
  if (c->dmabuf)
    zwp_linux_dmabuf_v1_add_listener(c->dmabuf, &kDmabufListener, c.get());
  if (c->seat) wl_seat_add_listener(c->seat, &kSeatListener, c.get());
  wl_display_roundtrip(c->display);  // formats, seat capabilities
  LOGI(
    "wayland: connected to %s: dmabuf=%s (XBGR8888 linear: %s), viewporter=%s, "
    "fractional-scale=%s, xdg-decoration=%s, presentation=%s, activation=%s, "
    "seat=%s",
    socketPath.c_str(), c->dmabuf ? "v3" : "no",
    c->linearDmabufFormats.count(kDrmXBGR8888) ? "yes" : "no",
    c->viewporter ? "yes" : "no", c->fractional ? "yes" : "no",
    c->decorations ? "yes" : "no", c->presentation ? "yes" : "no",
    c->activation ? "yes" : "no", c->seat ? "yes" : "no"
  );

  Impl* raw = c.get();
  c->input.setCommandHandler([raw](const std::string& l) {
    raw->onCommand(l);
  });
  c->input.start(inputPath);
  c->wakeFd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
  c->thread = std::thread([raw] { raw->loop(); });
  gConnection = new WaylandConnection(std::move(c));
  return true;
}

// ---- window ----------------------------------------------------------------

WaylandWindow::WaylandWindow(
  WaylandConnection::Impl* conn, WindowSpec spec, int64_t hwcId
)
    : c(conn), mSpec(std::move(spec)), mHwcId(hwcId) {
  // Start at the display's size in density-independent pixels (the
  // compositor scales); the window manager then matches the display to the
  // window's real size and scale.
  const double k = double(kBaseDpi) / std::max(mSpec.dpi, 1);
  mWinW = std::max(1, int32_t(mSpec.width * k + 0.5));
  mWinH = std::max(1, int32_t(mSpec.height * k + 0.5));
}

bool WaylandWindow::wantDmabuf() const {
  return c->dmabuf && c->linearDmabufFormats.count(kDrmXBGR8888) &&
         getIntProperty("vendor.linuxapk.wayland.force_shm", 0) == 0;
}

bool WaylandWindow::init() {
  mCtx = new Ctx{c, this};  // outlives the wl objects (see Ctx)
  if (!(wantDmabuf() && createPool(mSpec.width, mSpec.height, true, &mPool))) {
    if (!createPool(mSpec.width, mSpec.height, false, &mPool)) {
      delete mCtx;
      mCtx = nullptr;
      return false;
    }
  }
  std::lock_guard lock(c->mutex);
  for (auto& b : mPool.bufs)
    wl_buffer_add_listener(b.buffer, &kBufferListener, mCtx);
  mSurface = wl_compositor_create_surface(c->compositor);
  mXdgSurface = xdg_wm_base_get_xdg_surface(c->wmBase, mSurface);
  xdg_surface_add_listener(mXdgSurface, &kXdgSurfaceListener, mCtx);
  mToplevel = xdg_surface_get_toplevel(mXdgSurface);
  xdg_toplevel_add_listener(mToplevel, &kToplevelListener, mCtx);
  xdg_toplevel_set_title(mToplevel, mSpec.title.c_str());
  xdg_toplevel_set_app_id(mToplevel, mSpec.appId.c_str());
  xdg_toplevel_set_min_size(
    mToplevel, std::max(240, mSpec.minW), std::max(160, mSpec.minH)
  );
  if (c->decorations) {
    mDecoration = zxdg_decoration_manager_v1_get_toplevel_decoration(
      c->decorations, mToplevel
    );
    zxdg_toplevel_decoration_v1_set_mode(
      mDecoration, ZXDG_TOPLEVEL_DECORATION_V1_MODE_SERVER_SIDE
    );
  }
  if (c->viewporter)
    mViewport = wp_viewporter_get_viewport(c->viewporter, mSurface);
  if (c->fractional) {
    mFractional = wp_fractional_scale_manager_v1_get_fractional_scale(
      c->fractional, mSurface
    );
    wp_fractional_scale_v1_add_listener(
      mFractional, &kFractionalListener, mCtx
    );
  }
  wl_surface_commit(mSurface);  // initial commit without a buffer -> configure
  LOGI(
    "wayland: window %d (\"%s\", %s) for hwc display %lld: %ux%u, %s buffers "
    "(%d, %s)",
    mSpec.id, mSpec.title.c_str(), mSpec.appId.c_str(), (long long)mHwcId,
    mSpec.width, mSpec.height, mPool.dmabuf ? "linux-dmabuf" : "wl_shm",
    int(mPool.bufs.size()), mPool.bgrx ? "XRGB8888" : "XBGR8888"
  );
  wl_display_flush(c->display);
  return true;
}

bool WaylandWindow::createPool(uint32_t w, uint32_t h, bool dmabuf, Pool* out) {
  Pool pool;
  pool.width = w;
  pool.height = h;
  pool.stride = uint32_t(alignUp(uint64_t(w) * 4, 256));
  pool.dmabuf = dmabuf;
  const size_t size = alignUp(uint64_t(pool.stride) * h, kPageSize);
  pool.bufs.resize(kPoolSize);
  wl_event_queue* q = dmabuf ? wl_display_create_queue(c->display) : nullptr;
  bool ok = true;
  for (auto& b : pool.bufs) {
    b.size = size;
    b.memfd = memfd_create("linuxapk-wl", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    if (b.memfd < 0 || ftruncate(b.memfd, off_t(size)) != 0) {
      ok = false;
      break;
    }
    b.map = static_cast<uint8_t*>(
      mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, b.memfd, 0)
    );
    if (b.map == MAP_FAILED) {
      b.map = nullptr;
      ok = false;
      break;
    }
    if (dmabuf) {
      int dev = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
      if (dev < 0) {
        ok = false;
        break;
      }
      fcntl(b.memfd, F_ADD_SEALS, F_SEAL_SHRINK);
      udmabuf_create create{};
      create.memfd = uint32_t(b.memfd);
      create.flags = UDMABUF_FLAGS_CLOEXEC;
      create.size = size;
      b.dmabuf = ioctl(dev, UDMABUF_CREATE, &create);
      close(dev);
      if (b.dmabuf < 0) {
        ok = false;
        break;
      }
      // Async create (a create_immed failure would kill the connection).
      auto* wrapper =
        static_cast<zwp_linux_dmabuf_v1*>(wl_proxy_create_wrapper(c->dmabuf));
      wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(wrapper), q);
      zwp_linux_buffer_params_v1* params =
        zwp_linux_dmabuf_v1_create_params(wrapper);
      wl_proxy_wrapper_destroy(wrapper);
      struct Result {
        wl_buffer* buffer = nullptr;
        bool done = false;
      } res;
      static const zwp_linux_buffer_params_v1_listener kParams = {
        [](void* d, zwp_linux_buffer_params_v1*, wl_buffer* buf) {
          auto* r = static_cast<Result*>(d);
          r->buffer = buf;
          r->done = true;
        },
        [](void* d, zwp_linux_buffer_params_v1*) {
          static_cast<Result*>(d)->done = true;
        }
      };
      zwp_linux_buffer_params_v1_add_listener(params, &kParams, &res);
      zwp_linux_buffer_params_v1_add(params, b.dmabuf, 0, 0, pool.stride, 0, 0);
      zwp_linux_buffer_params_v1_create(
        params, int32_t(w), int32_t(h), kDrmXBGR8888, 0
      );
      while (!res.done && wl_display_roundtrip_queue(c->display, q) >= 0) {
      }
      zwp_linux_buffer_params_v1_destroy(params);
      if (!res.buffer) {
        LOGW("wayland: compositor rejected the linear dma-buf; using wl_shm");
        ok = false;
        break;
      }
      wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(res.buffer), nullptr);
      b.buffer = res.buffer;
    } else {
      // XRGB8888 (B,G,R,X bytes) is the one format every compositor
      // must support; use XBGR8888 (no swizzle) when offered.
      pool.bgrx = !c->shmFormats.count(WL_SHM_FORMAT_XBGR8888);
      wl_shm_pool* sp = wl_shm_create_pool(c->shm, b.memfd, int32_t(size));
      b.buffer = wl_shm_pool_create_buffer(
        sp, 0, int32_t(w), int32_t(h), int32_t(pool.stride),
        pool.bgrx ? WL_SHM_FORMAT_XRGB8888 : WL_SHM_FORMAT_XBGR8888
      );
      wl_shm_pool_destroy(sp);
    }
  }
  if (q) wl_event_queue_destroy(q);
  if (!ok) {
    pool.destroy();
    return false;
  }
  *out = std::move(pool);
  return true;
}

WaylandWindow::~WaylandWindow() {
  std::lock_guard lock(c->mutex);
  if (mCtx) mCtx->w = nullptr;  // pending events for our objects now no-op
  mPool.destroy();
  for (auto& p : mRetired) p.destroy();
  if (mFractional) wp_fractional_scale_v1_destroy(mFractional);
  if (mViewport) wp_viewport_destroy(mViewport);
  if (mDecoration) zxdg_toplevel_decoration_v1_destroy(mDecoration);
  if (mToplevel) xdg_toplevel_destroy(mToplevel);
  if (mXdgSurface) xdg_surface_destroy(mXdgSurface);
  if (mSurface) wl_surface_destroy(mSurface);
  wl_display_flush(c->display);
  // mCtx is leaked on purpose: queued events may still reference it.
}

void WaylandWindow::setTitle(const std::string& title) {
  std::lock_guard lock(c->mutex);
  mSpec.title = title;
  if (mToplevel) xdg_toplevel_set_title(mToplevel, title.c_str());
  wl_display_flush(c->display);
}

void WaylandWindow::setMinSize(int32_t w, int32_t h) {
  std::lock_guard lock(c->mutex);
  mSpec.minW = w;
  mSpec.minH = h;
  if (mToplevel)
    xdg_toplevel_set_min_size(mToplevel, std::max(240, w), std::max(160, h));
  if (mSurface) wl_surface_commit(mSurface);
  wl_display_flush(c->display);
}

// Asks the compositor to focus the window (xdg_activation_v1). Compositors
// may refuse without a recent input serial (focus-stealing prevention); KWin
// then marks the window as demanding attention.
void WaylandWindow::activate() {
  std::lock_guard lock(c->mutex);
  if (!c->activation || !mSurface) return;
  xdg_activation_token_v1* t =
    xdg_activation_v1_get_activation_token(c->activation);
  xdg_activation_token_v1_add_listener(t, &kTokenListener, mCtx);
  if (c->lastSerial && c->seat)
    xdg_activation_token_v1_set_serial(t, c->lastSerial, c->seat);
  auto focused = c->windows.find(c->keyboardFocus);
  if (focused != c->windows.end())
    xdg_activation_token_v1_set_surface(t, focused->second->surface());
  xdg_activation_token_v1_set_app_id(t, mSpec.appId.c_str());
  xdg_activation_token_v1_commit(t);
  wl_display_flush(c->display);
}

void WaylandWindow::onActivationToken(const char* token) {
  xdg_activation_v1_activate(c->activation, token, mSurface);
  wl_display_flush(c->display);
}

void WaylandWindow::onSurfaceConfigure(uint32_t serial) {
  xdg_surface_ack_configure(mXdgSurface, serial);
  if (mPendingW > 0 && mPendingH > 0) {
    mWinW = mPendingW;
    mWinH = mPendingH;
  }
  mConfigured = true;
  updateViewportLocked();
  c->input.setWindow(mSpec.id, mWinW, mWinH, mScale);
  // (Re)show the newest frame at the new size; the first configure maps the
  // window.
  if (mLast >= 0) {
    commitLocked(mLast);
  } else {
    wl_surface_commit(mSurface);
  }
}

void WaylandWindow::onConfigure(int32_t w, int32_t h) {
  mPendingW = w;
  mPendingH = h;
}

void WaylandWindow::updateViewportLocked() {
  if (mViewport) wp_viewport_set_destination(mViewport, mWinW, mWinH);
}

void WaylandWindow::onRelease(wl_buffer* buf) {
  for (auto& b : mPool.bufs) {
    if (b.buffer == buf) b.busy = false;
  }
}

void WaylandWindow::onScale(uint32_t scale120) {
  mScale = scale120 / 120.0;
  if (mConfigured) c->input.setWindow(mSpec.id, mWinW, mWinH, mScale);
}

void WaylandWindow::onPresented(bool presented, uint32_t refreshNs) {
  if (presented) {
    mPresented++;
    mRefreshNs = refreshNs;
  } else {
    mDiscarded++;
  }
}

void WaylandWindow::mapToDisplay(
  double sx, double sy, double* x, double* y
) const {
  // The viewport stretches the shown frame over the whole surface.
  *x = mWinW > 0 ? sx * mPool.width / mWinW : sx;
  *y = mWinH > 0 ? sy * mPool.height / mWinH : sy;
}

void WaylandWindow::commitLocked(int index) {
  PoolBuffer& b = mPool.bufs[size_t(index)];
  wl_surface_attach(mSurface, b.buffer, 0, 0);
  wl_surface_damage_buffer(
    mSurface, 0, 0, int32_t(mPool.width), int32_t(mPool.height)
  );
  if (c->presentation) {
    struct wp_presentation_feedback* f =
      wp_presentation_feedback(c->presentation, mSurface);
    wp_presentation_feedback_add_listener(f, &kFeedbackListener, mCtx);
  }
  wl_surface_commit(mSurface);
  b.busy = true;
  // Older pools aren't attached any more; the compositor keeps whatever it
  // still reads (it holds its own reference to the memory).
  for (auto& p : mRetired) p.destroy();
  mRetired.clear();
}

Error WaylandWindow::present(FrameInput& frame) {
  ClientTargetState* ct = frame.clientTarget;
  BufferPtr buffer = ct && ct->buffer ? ct->buffer : frame.outputBuffer;
  if (!buffer) return Error::None;
  const BufferHandle* bh = buffer->info();
  const uint8_t* src = buffer->map();
  if (!bh || !src) return Error::None;
  const int32_t f = bh->resolvedFormat;
  if (f != fmt::RGBA_8888 && f != fmt::RGBX_8888 && f != fmt::BGRA_8888) {
    if (!mWarnedFormat)
      LOGW("wayland: client target format 0x%x not supported yet", f);
    mWarnedFormat = true;
    return Error::None;
  }
  if (ct && ct->buffer == buffer && !waitFence(ct->acquireFence.get(), 1000))
    LOGW("wayland: client target fence timeout");
  src += bh->planes[0].offset.get();
  const uint32_t srcStride = bh->planes[0].stride;
  // A virtual display's frames follow its size (VirtualDisplay.resize);
  // display 0's size is fixed.
  const uint32_t w = mHwcId >= 0 ? bh->width : std::min(bh->width, mSpec.width);
  const uint32_t h =
    mHwcId >= 0 ? bh->height : std::min(bh->height, mSpec.height);
  if (w == 0 || h == 0) return Error::None;
  if (w != mPool.width || h != mPool.height) {
    // Build the new pool without the lock (dma-buf creation round-trips).
    Pool np;
    if (
      !(wantDmabuf() && createPool(w, h, true, &np)) &&
      !createPool(w, h, false, &np)
    ) {
      LOGE("wayland: window %d: no %ux%u buffers", mSpec.id, w, h);
      return Error::None;
    }
    std::lock_guard lock(c->mutex);
    for (auto& b : np.bufs)
      wl_buffer_add_listener(b.buffer, &kBufferListener, mCtx);
    LOGI(
      "wayland: window %d: frames now %ux%u (were %ux%u)", mSpec.id, w, h,
      mPool.width, mPool.height
    );
    mRetired.push_back(std::move(mPool));
    mPool = std::move(np);
    mLast = -1;
  }

  std::lock_guard lock(c->mutex);
  mFrames++;
  int idx = -1;
  for (size_t i = 0; i < mPool.bufs.size(); i++) {
    if (!mPool.bufs[i].busy && int(i) != mLast) {
      idx = int(i);
      break;
    }
  }
  if (idx < 0) {
    // Every buffer is still held by the compositor: drop this frame
    // rather than overwrite one it may be reading.
    mDropped++;
    return Error::None;
  }
  uint8_t* dst = mPool.bufs[size_t(idx)].map;
  for (uint32_t y = 0; y < h; y++) {
    const uint8_t* s = src + size_t(y) * srcStride;
    uint8_t* d = dst + size_t(y) * mPool.stride;
    if ((f == fmt::BGRA_8888) != mPool.bgrx) {  // swap R and B
      for (uint32_t x = 0; x < w; x++) {
        d[4 * x] = s[4 * x + 2];
        d[4 * x + 1] = s[4 * x + 1];
        d[4 * x + 2] = s[4 * x];
        d[4 * x + 3] = 0xff;
      }
    } else {
      memcpy(d, s, size_t(w) * 4);
    }
  }
  mLast = idx;
  if (mConfigured) commitLocked(idx);
  const int64_t now = nowNs();
  if (now - mLastLogNs > 10'000'000'000) {
    LOGI(
      "wayland: window %d: %llu frames, %llu shown by compositor, %llu "
      "discarded, %llu dropped (all buffers busy), frame %ux%u, window %dx%d "
      "scale %.2f, refresh %.2f Hz",
      mSpec.id, (unsigned long long)mFrames, (unsigned long long)mPresented,
      (unsigned long long)mDiscarded, (unsigned long long)mDropped, mPool.width,
      mPool.height, mWinW, mWinH, mScale, mRefreshNs ? 1e9 / mRefreshNs : 0.0
    );
    mLastLogNs = now;
  }
  wl_display_flush(c->display);
  return Error::None;
}

// ---- presenters ------------------------------------------------------------

namespace {

// Shows one display in one window of the registry. The window may go away
// first (window_destroy), after which frames are only dumped.
class WindowPresenter : public Presenter {
 public:
  WindowPresenter(
    WaylandConnection::Impl* c, int32_t windowId, int64_t hwcId,
    std::unique_ptr<HeadlessPresenter> secondary
  )
      : c(c),
        mWindowId(windowId),
        mHwcId(hwcId),
        mSecondary(std::move(secondary)) {}
  ~WindowPresenter() override { c->removeWindow(mWindowId, mHwcId); }
  const char* name() const override { return "wayland"; }
  Error present(FrameInput& frame, PresentResult* out) override {
    // Both copy synchronously: SurfaceFlinger may reuse its buffer right away.
    if (mSecondary) {
      PresentResult ignored;
      mSecondary->present(frame, &ignored);
    }
    out->presentFence.reset();
    if (auto w = c->findWindow(mWindowId, mHwcId)) return w->present(frame);
    return Error::None;
  }

 private:
  WaylandConnection::Impl* c;
  const int32_t mWindowId;
  const int64_t mHwcId;
  std::unique_ptr<HeadlessPresenter> mSecondary;
};

}  // namespace

void setPrimaryDisplaySize(uint32_t width, uint32_t height) {
  if (WaylandConnection* conn = WaylandConnection::get())
    conn->impl()->input.setDisplaySize(width, height);
}

std::unique_ptr<Presenter> createWaylandPresenter(
  int64_t displayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
) {
  WaylandConnection* conn = WaylandConnection::get();
  if (!conn) return nullptr;
  WaylandConnection::Impl* c = conn->impl();
  WindowSpec spec;
  spec.id = int32_t(displayId);
  spec.width = width;
  spec.height = height;
  spec.dpi = kBaseDpi;  // the window starts at the display's pixel size
  spec.appId = "linuxapk";
  spec.title = "Linux.apk";
  auto w = std::make_shared<WaylandWindow>(c, spec, /*hwcId=*/-1);
  if (!w->init()) return nullptr;
  c->addWindow(w);
  return std::make_unique<WindowPresenter>(
    c, spec.id, -1, std::move(secondary)
  );
}

std::unique_ptr<Presenter> createWaylandVirtualPresenter(
  int64_t hwcDisplayId, uint32_t width, uint32_t height,
  std::unique_ptr<HeadlessPresenter> secondary
) {
  WaylandConnection* conn = WaylandConnection::get();
  if (!conn) return nullptr;
  WaylandConnection::Impl* c = conn->impl();
  std::optional<WindowSpec> spec = c->claimPending(width, height);
  if (!spec) {
    LOGI(
      "virtual display %lld (%ux%u): no window requested; headless",
      (long long)hwcDisplayId, width, height
    );
    return nullptr;
  }
  auto w = std::make_shared<WaylandWindow>(c, *spec, hwcDisplayId);
  if (!w->init()) {
    c->input.send("window_failed %d wayland", spec->id);
    return nullptr;
  }
  c->addWindow(w);
  c->input.send("window_mapped %d %lld", spec->id, (long long)hwcDisplayId);
  if (secondary) {
    char prefix[32];
    snprintf(prefix, sizeof prefix, "window%d", spec->id);
    secondary->setDumpPrefix(prefix);
  }
  return std::make_unique<WindowPresenter>(
    c, spec->id, hwcDisplayId, std::move(secondary)
  );
}

}  // namespace linuxapk::hwc
