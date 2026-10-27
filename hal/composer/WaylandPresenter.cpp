// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <atomic>
#include <mutex>
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
#include "xdg-decoration-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

namespace linuxapk::hwc {

namespace {
constexpr uint32_t kDrmXBGR8888 =
  fourcc('X', 'B', '2', '4');  // R,G,B,X bytes = RGBA_8888
constexpr uint64_t kModLinear = 0;
constexpr int kPoolSize = 3;

template <typename T>
T* bindGlobal(
  wl_registry* r, uint32_t name, const wl_interface* iface, uint32_t have,
  uint32_t want
) {
  return static_cast<T*>(
    wl_registry_bind(r, name, iface, std::min(have, want))
  );
}
}  // namespace

// ---------------------------------------------------------------------------

struct PoolBuffer {
  int memfd = -1;
  int dmabuf = -1;
  uint8_t* map = nullptr;
  size_t size = 0;
  wl_buffer* buffer = nullptr;
  bool busy = false;  // attached and not yet released by the compositor
};

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
  wl_seat* seat = nullptr;
  wl_pointer* pointer = nullptr;
  wl_keyboard* keyboard = nullptr;
  wl_touch* touch = nullptr;

  std::set<uint32_t> linearDmabufFormats;
  std::set<uint32_t> shmFormats;

  std::mutex mutex;  // guards window state shared with the event thread
  WaylandWindow* window = nullptr;
  InputServer input;

  int wakeFd = -1;
  std::thread thread;
  std::atomic<bool> stop{false};

  ~Impl();
  void loop();
  // Surface-local logical -> display pixels.
  void toDisplay(wl_fixed_t sx, wl_fixed_t sy, double* x, double* y);
};

class WaylandWindow {
 public:
  WaylandWindow(
    WaylandConnection::Impl* c, int64_t displayId, uint32_t w, uint32_t h
  );
  ~WaylandWindow();
  bool init();
  Error present(FrameInput& frame);

  // Called with c->mutex held.
  void onConfigure(int32_t w, int32_t h);
  void onSurfaceConfigure(uint32_t serial);
  void onRelease(wl_buffer* b);
  void onScale(uint32_t scale120);
  void onPresented(bool presented, uint32_t refreshNs);
  void mapToDisplay(double sx, double sy, double* x, double* y) const;

 private:
  bool createPool(bool dmabuf);
  void destroyPool();
  void commitLocked(int index);
  void updateViewportLocked();

  WaylandConnection::Impl* c;
  const int64_t mDisplayId;
  const uint32_t mWidth, mHeight;
  uint32_t mStride = 0;
  wl_surface* mSurface = nullptr;
  xdg_surface* mXdgSurface = nullptr;
  xdg_toplevel* mToplevel = nullptr;
  wp_viewport* mViewport = nullptr;
  wp_fractional_scale_v1* mFractional = nullptr;
  zxdg_toplevel_decoration_v1* mDecoration = nullptr;
  std::vector<PoolBuffer> mPool;
  bool mDmabuf = false;
  bool mBgrx = false;  // pool pixels are B,G,R,X (else R,G,B,X)
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

// Pointer, keyboard, touch -> InputServer.
#define C static_cast<WaylandConnection::Impl*>(data)
static void ptrEnter(
  void* data, wl_pointer*, uint32_t, wl_surface*, wl_fixed_t sx, wl_fixed_t sy
) {
  double x, y;
  C->toDisplay(sx, sy, &x, &y);
  C->input.send("pointer_enter %.2f %.2f", x, y);
}
static void ptrLeave(void* data, wl_pointer*, uint32_t, wl_surface*) {
  C->input.send("pointer_leave");
}
static void ptrMotion(
  void* data, wl_pointer*, uint32_t t, wl_fixed_t sx, wl_fixed_t sy
) {
  double x, y;
  C->toDisplay(sx, sy, &x, &y);
  C->input.send("pointer_motion %u %.2f %.2f", t, x, y);
}
static void ptrButton(
  void* data, wl_pointer*, uint32_t, uint32_t t, uint32_t button, uint32_t state
) {
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
  void* data, wl_keyboard*, uint32_t, wl_surface*, wl_array*
) {
  C->input.send("keyboard_enter");
}
static void kbLeave(void* data, wl_keyboard*, uint32_t, wl_surface*) {
  C->input.send("keyboard_leave");
}
static void kbKey(
  void* data, wl_keyboard*, uint32_t, uint32_t t, uint32_t key, uint32_t state
) {
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
  void* data, wl_touch*, uint32_t, uint32_t t, wl_surface*, int32_t id,
  wl_fixed_t sx, wl_fixed_t sy
) {
  double x, y;
  C->toDisplay(sx, sy, &x, &y);
  C->input.send("touch_down %u %d %.2f %.2f", t, id, x, y);
}
static void tUp(void* data, wl_touch*, uint32_t, uint32_t t, int32_t id) {
  C->input.send("touch_up %u %d", t, id);
}
static void tMotion(
  void* data, wl_touch*, uint32_t t, int32_t id, wl_fixed_t sx, wl_fixed_t sy
) {
  double x, y;
  C->toDisplay(sx, sy, &x, &y);
  C->input.send("touch_motion %u %d %.2f %.2f", t, id, x, y);
}
static void tFrame(void* data, wl_touch*) { C->input.send("touch_frame"); }
static void tCancel(void* data, wl_touch*) { C->input.send("touch_cancel"); }
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

// Window listeners: data is the WaylandWindow; they take the connection lock.
struct WindowCtx {
  WaylandConnection::Impl* c;
  WaylandWindow* w;
};

static void xdgSurfaceConfigure(void* data, xdg_surface*, uint32_t serial) {
  auto* ctx = static_cast<WindowCtx*>(data);
  std::lock_guard lock(ctx->c->mutex);
  ctx->w->onSurfaceConfigure(serial);
}
static const xdg_surface_listener kXdgSurfaceListener = {xdgSurfaceConfigure};

static void toplevelConfigure(
  void* data, xdg_toplevel*, int32_t w, int32_t h, wl_array*
) {
  auto* ctx = static_cast<WindowCtx*>(data);
  std::lock_guard lock(ctx->c->mutex);
  ctx->w->onConfigure(w, h);
}
static void toplevelClose(void* data, xdg_toplevel*) {
  static_cast<WindowCtx*>(data)->c->input.send("close");
}
static void toplevelBounds(void*, xdg_toplevel*, int32_t, int32_t) {}
static void toplevelCaps(void*, xdg_toplevel*, wl_array*) {}
static const xdg_toplevel_listener kToplevelListener = {
  toplevelConfigure, toplevelClose, toplevelBounds, toplevelCaps
};

static void bufferRelease(void* data, wl_buffer* b) {
  auto* ctx = static_cast<WindowCtx*>(data);
  std::lock_guard lock(ctx->c->mutex);
  ctx->w->onRelease(b);
}
static const wl_buffer_listener kBufferListener = {bufferRelease};

static void preferredScale(
  void* data, wp_fractional_scale_v1*, uint32_t scale120
) {
  auto* ctx = static_cast<WindowCtx*>(data);
  std::lock_guard lock(ctx->c->mutex);
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
  auto* ctx = static_cast<WindowCtx*>(data);
  {
    std::lock_guard lock(ctx->c->mutex);
    if (ctx->c->window == ctx->w) ctx->w->onPresented(true, refresh);
  }
  wp_presentation_feedback_destroy(f);
}
static void fbDiscarded(void* data, struct wp_presentation_feedback* f) {
  auto* ctx = static_cast<WindowCtx*>(data);
  {
    std::lock_guard lock(ctx->c->mutex);
    if (ctx->c->window == ctx->w) ctx->w->onPresented(false, 0);
  }
  wp_presentation_feedback_destroy(f);
}
static const wp_presentation_feedback_listener kFeedbackListener = {
  fbSyncOutput, fbPresented, fbDiscarded
};

// ---- connection
// --------------------------------------------------------------

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

void WaylandConnection::Impl::toDisplay(
  wl_fixed_t sx, wl_fixed_t sy, double* x, double* y
) {
  std::lock_guard lock(mutex);
  double fx = wl_fixed_to_double(sx), fy = wl_fixed_to_double(sy);
  if (window) {
    window->mapToDisplay(fx, fy, x, y);
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
    "fractional-scale=%s, xdg-decoration=%s, presentation=%s, seat=%s",
    socketPath.c_str(), c->dmabuf ? "v3" : "no",
    c->linearDmabufFormats.count(kDrmXBGR8888) ? "yes" : "no",
    c->viewporter ? "yes" : "no", c->fractional ? "yes" : "no",
    c->decorations ? "yes" : "no", c->presentation ? "yes" : "no",
    c->seat ? "yes" : "no"
  );

  c->input.start(inputPath);
  c->wakeFd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
  Impl* raw = c.get();
  c->thread = std::thread([raw] { raw->loop(); });
  gConnection = new WaylandConnection(std::move(c));
  return true;
}

std::unique_ptr<WaylandWindow> WaylandConnection::createWindow(
  int64_t displayId, uint32_t w, uint32_t h
) {
  auto win = std::make_unique<WaylandWindow>(mImpl.get(), displayId, w, h);
  if (!win->init()) return nullptr;
  return win;
}

// ---- window
// ------------------------------------------------------------------

WaylandWindow::WaylandWindow(
  WaylandConnection::Impl* conn, int64_t displayId, uint32_t w, uint32_t h
)
    : c(conn),
      mDisplayId(displayId),
      mWidth(w),
      mHeight(h),
      mWinW(int32_t(w)),
      mWinH(int32_t(h)) {}

bool WaylandWindow::init() {
  auto* ctx =
    new WindowCtx{c, this};  // lives as long as the wl objects (freed in dtor)
  mStride = uint32_t(alignUp(uint64_t(mWidth) * 4, 256));
  bool wantDmabuf = c->dmabuf && c->linearDmabufFormats.count(kDrmXBGR8888) &&
                    getIntProperty("vendor.linuxapk.wayland.force_shm", 0) == 0;
  if (!(wantDmabuf && createPool(true))) {
    if (!createPool(false)) {
      delete ctx;
      return false;
    }
  }
  std::lock_guard lock(c->mutex);
  for (auto& b : mPool) wl_buffer_add_listener(b.buffer, &kBufferListener, ctx);
  mSurface = wl_compositor_create_surface(c->compositor);
  wl_surface_set_user_data(mSurface, ctx);
  mXdgSurface = xdg_wm_base_get_xdg_surface(c->wmBase, mSurface);
  xdg_surface_add_listener(mXdgSurface, &kXdgSurfaceListener, ctx);
  mToplevel = xdg_surface_get_toplevel(mXdgSurface);
  xdg_toplevel_add_listener(mToplevel, &kToplevelListener, ctx);
  xdg_toplevel_set_title(mToplevel, "Linux.apk");
  xdg_toplevel_set_app_id(mToplevel, "linuxapk");
  xdg_toplevel_set_min_size(mToplevel, 160, 100);
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
    wp_fractional_scale_v1_add_listener(mFractional, &kFractionalListener, ctx);
  }
  wl_surface_commit(mSurface);  // initial commit without a buffer -> configure
  c->window = this;
  c->input.setDisplaySize(mWidth, mHeight);
  LOGI(
    "wayland: window for display %lld: %ux%u, %s buffers (%d, %s), stride %u",
    (long long)mDisplayId, mWidth, mHeight, mDmabuf ? "linux-dmabuf" : "wl_shm",
    int(mPool.size()), mBgrx ? "XRGB8888" : "XBGR8888", mStride
  );
  wl_display_flush(c->display);
  return true;
}

bool WaylandWindow::createPool(bool dmabuf) {
  const size_t size = alignUp(uint64_t(mStride) * mHeight, kPageSize);
  std::vector<PoolBuffer> pool(kPoolSize);
  wl_event_queue* q = dmabuf ? wl_display_create_queue(c->display) : nullptr;
  bool ok = true;
  for (auto& b : pool) {
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
      zwp_linux_buffer_params_v1_add(params, b.dmabuf, 0, 0, mStride, 0, 0);
      zwp_linux_buffer_params_v1_create(
        params, int32_t(mWidth), int32_t(mHeight), kDrmXBGR8888, 0
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
      mBgrx = !c->shmFormats.count(WL_SHM_FORMAT_XBGR8888);
      wl_shm_pool* sp = wl_shm_create_pool(c->shm, b.memfd, int32_t(size));
      b.buffer = wl_shm_pool_create_buffer(
        sp, 0, int32_t(mWidth), int32_t(mHeight), int32_t(mStride),
        mBgrx ? WL_SHM_FORMAT_XRGB8888 : WL_SHM_FORMAT_XBGR8888
      );
      wl_shm_pool_destroy(sp);
    }
  }
  if (q) wl_event_queue_destroy(q);
  if (!ok) {
    for (auto& b : pool) {
      if (b.buffer) wl_buffer_destroy(b.buffer);
      if (b.map) munmap(b.map, b.size);
      if (b.dmabuf >= 0) close(b.dmabuf);
      if (b.memfd >= 0) close(b.memfd);
    }
    return false;
  }
  mPool = std::move(pool);
  mDmabuf = dmabuf;
  return true;
}

WaylandWindow::~WaylandWindow() {
  WindowCtx* ctx = nullptr;
  {
    std::lock_guard lock(c->mutex);
    if (c->window == this) c->window = nullptr;
    if (mSurface)
      ctx = static_cast<WindowCtx*>(wl_surface_get_user_data(mSurface));
    for (auto& b : mPool) {
      if (b.buffer) wl_buffer_destroy(b.buffer);
      if (b.map) munmap(b.map, b.size);
      if (b.dmabuf >= 0) close(b.dmabuf);
      if (b.memfd >= 0) close(b.memfd);
    }
    if (mFractional) wp_fractional_scale_v1_destroy(mFractional);
    if (mViewport) wp_viewport_destroy(mViewport);
    if (mDecoration) zxdg_toplevel_decoration_v1_destroy(mDecoration);
    if (mToplevel) xdg_toplevel_destroy(mToplevel);
    if (mXdgSurface) xdg_surface_destroy(mXdgSurface);
    if (mSurface) wl_surface_destroy(mSurface);
    wl_display_flush(c->display);
  }
  // Pending events for destroyed proxies are dropped by libwayland; ctx may
  // still be referenced by in-flight feedback objects, so it is leaked.
  (void)ctx;
}

void WaylandWindow::onSurfaceConfigure(uint32_t serial) {
  xdg_surface_ack_configure(mXdgSurface, serial);
  if (mPendingW > 0 && mPendingH > 0) {
    mWinW = mPendingW;
    mWinH = mPendingH;
  }
  mConfigured = true;
  updateViewportLocked();
  c->input.setWindow(mWinW, mWinH, mScale);
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
  for (auto& b : mPool) {
    if (b.buffer == buf) b.busy = false;
  }
}

void WaylandWindow::onScale(uint32_t scale120) {
  mScale = scale120 / 120.0;
  c->input.setWindow(mWinW, mWinH, mScale);
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
  // The viewport stretches the display over the whole surface.
  *x = mWinW > 0 ? sx * mWidth / mWinW : sx;
  *y = mWinH > 0 ? sy * mHeight / mWinH : sy;
}

void WaylandWindow::commitLocked(int index) {
  PoolBuffer& b = mPool[size_t(index)];
  wl_surface_attach(mSurface, b.buffer, 0, 0);
  wl_surface_damage_buffer(mSurface, 0, 0, int32_t(mWidth), int32_t(mHeight));
  if (c->presentation) {
    auto* ctx = static_cast<WindowCtx*>(wl_surface_get_user_data(mSurface));
    struct wp_presentation_feedback* f =
      wp_presentation_feedback(c->presentation, mSurface);
    wp_presentation_feedback_add_listener(f, &kFeedbackListener, ctx);
  }
  wl_surface_commit(mSurface);
  b.busy = true;
}

Error WaylandWindow::present(FrameInput& frame) {
  ClientTargetState* ct = frame.clientTarget;
  if (!ct || !ct->buffer) return Error::None;
  const BufferHandle* bh = ct->buffer->info();
  const uint8_t* src = ct->buffer->map();
  if (!bh || !src) return Error::None;
  const int32_t f = bh->resolvedFormat;
  if (f != fmt::RGBA_8888 && f != fmt::RGBX_8888 && f != fmt::BGRA_8888) {
    if (!mWarnedFormat)
      LOGW("wayland: client target format 0x%x not supported yet", f);
    mWarnedFormat = true;
    return Error::None;
  }
  if (!waitFence(ct->acquireFence.get(), 1000))
    LOGW("wayland: client target fence timeout");
  src += bh->planes[0].offset.get();
  const uint32_t srcStride = bh->planes[0].stride;
  const uint32_t w = std::min(bh->width, mWidth),
                 h = std::min(bh->height, mHeight);

  std::lock_guard lock(c->mutex);
  mFrames++;
  int idx = -1;
  for (size_t i = 0; i < mPool.size(); i++) {
    if (!mPool[i].busy && int(i) != mLast) {
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
  uint8_t* dst = mPool[size_t(idx)].map;
  for (uint32_t y = 0; y < h; y++) {
    const uint8_t* s = src + size_t(y) * srcStride;
    uint8_t* d = dst + size_t(y) * mStride;
    if ((f == fmt::BGRA_8888) != mBgrx) {  // swap R and B
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
      "wayland: display %lld: %llu frames, %llu shown by compositor, %llu "
      "discarded, "
      "%llu dropped (all buffers busy), window %dx%d scale %.2f, refresh %.2f "
      "Hz",
      (long long)mDisplayId, (unsigned long long)mFrames,
      (unsigned long long)mPresented, (unsigned long long)mDiscarded,
      (unsigned long long)mDropped, mWinW, mWinH, mScale,
      mRefreshNs ? 1e9 / mRefreshNs : 0.0
    );
    mLastLogNs = now;
  }
  wl_display_flush(c->display);
  return Error::None;
}

// ---- presenter
// -----------------------------------------------------------------

std::unique_ptr<Presenter> createWaylandPresenter(
  int64_t displayId, uint32_t width, uint32_t height,
  std::unique_ptr<Presenter> secondary
) {
  WaylandConnection* c = WaylandConnection::get();
  if (!c) return nullptr;
  auto window = c->createWindow(displayId, width, height);
  if (!window) return nullptr;
  return std::make_unique<WaylandPresenter>(
    std::move(window), std::move(secondary)
  );
}

WaylandPresenter::WaylandPresenter(
  std::unique_ptr<WaylandWindow> window, std::unique_ptr<Presenter> secondary
)
    : mWindow(std::move(window)), mSecondary(std::move(secondary)) {}
WaylandPresenter::~WaylandPresenter() = default;

Error WaylandPresenter::present(FrameInput& frame, PresentResult* out) {
  // Both copy synchronously: SurfaceFlinger may reuse its buffer right away.
  if (mSecondary) {
    PresentResult ignored;
    mSecondary->present(frame, &ignored);
  }
  out->presentFence.reset();
  return mWindow->present(frame);
}

}  // namespace linuxapk::hwc
