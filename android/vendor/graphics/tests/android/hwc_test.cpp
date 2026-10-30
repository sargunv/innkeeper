// SPDX-License-Identifier: AGPL-3.0-or-later
// On-device composer3 test, acting like SurfaceFlinger: hotplug, vsync,
// validate (DEVICE -> CLIENT), accept, client target + present, slot cache.
// Needs servicemanager, our allocator and composer running.
#include <aidl/android/hardware/graphics/allocator/IAllocator.h>
#include <aidl/android/hardware/graphics/composer3/BnComposerCallback.h>
#include <aidl/android/hardware/graphics/composer3/IComposer.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <stdio.h>

#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#include <algorithm>
#include <atomic>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <fcntl.h>
#include <linux/dma-buf.h>
#include <linux/sync_file.h>
#include <poll.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include "linuxapk/gralloc_handle.h"

using namespace aidl::android::hardware::graphics::composer3;
using namespace aidl::android::hardware::graphics::allocator;
using aidl::android::hardware::common::NativeHandle;
using aidl::android::hardware::graphics::common::BufferUsage;
using aidl::android::hardware::graphics::common::Dataspace;
using aidl::android::hardware::graphics::common::DisplayHotplugEvent;
using aidl::android::hardware::graphics::common::PixelFormat;
using aidl::android::hardware::graphics::common::Rect;

static int gFail = 0, gChecks = 0;
#define CHECK(c)                                                   \
  do {                                                             \
    gChecks++;                                                     \
    if (!(c)) {                                                    \
      gFail++;                                                     \
      fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); \
    }                                                              \
  } while (0)

static int64_t nowNs() {
  timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return int64_t(ts.tv_sec) * 1000000000 + ts.tv_nsec;
}

class Callback : public BnComposerCallback {
 public:
  std::atomic<int> hotplugs{0}, vsyncs{0};
  std::mutex mutex;
  std::vector<int64_t> stamps;
  int32_t lastPeriod = 0;

  ndk::ScopedAStatus onHotplug(int64_t display, bool connected) override {
    printf("  onHotplug(%lld, %d)\n", (long long)display, connected);
    hotplugs++;
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus onHotplugEvent(
    int64_t display, DisplayHotplugEvent
  ) override {
    return onHotplug(display, true);
  }
  ndk::ScopedAStatus onVsync(int64_t, int64_t ts, int32_t period) override {
    std::lock_guard l(mutex);
    stamps.push_back(ts);
    lastPeriod = period;
    vsyncs++;
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus onRefresh(int64_t) override {
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus onSeamlessPossible(int64_t) override {
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus onVsyncPeriodTimingChanged(
    int64_t, const VsyncPeriodChangeTimeline&
  ) override {
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus onVsyncIdle(int64_t) override {
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus onRefreshRateChangedDebug(
    const RefreshRateChangedDebugData&
  ) override {
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus onHdcpLevelsChanged(
    int64_t, const aidl::android::hardware::drm::HdcpLevels&
  ) override {
    return ndk::ScopedAStatus::ok();
  }
};

static NativeHandle dupHandle(const NativeHandle& h) {
  NativeHandle out;
  for (auto& fd : h.fds) out.fds.emplace_back(dup(fd.get()));
  out.ints = h.ints;
  return out;
}

static int countErrors(const std::vector<CommandResultPayload>& r) {
  int n = 0;
  for (auto& p : r) {
    if (p.getTag() == CommandResultPayload::error) {
      auto& e = p.get<CommandResultPayload::error>();
      printf("  CommandError index=%d code=%d\n", e.commandIndex, e.errorCode);
      n++;
    }
  }
  return n;
}

template <CommandResultPayload::Tag T>
static const auto* find(const std::vector<CommandResultPayload>& r) {
  for (auto& p : r)
    if (p.getTag() == T) return &p.get<T>();
  return (const std::decay_t<decltype(r[0].get<T>())>*)nullptr;
}

static int producerFence(const NativeHandle& h) {
  dma_buf_export_sync_file fence{.flags = DMA_BUF_SYNC_READ, .fd = -1};
  CHECK(ioctl(h.fds[0].get(), DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &fence) == 0);
  return fence.fd;
}

static void checkFence(int fd) {
  CHECK(fd >= 0);
  if (fd < 0) return;
  sync_file_info info{};
  CHECK(ioctl(fd, SYNC_IOC_FILE_INFO, &info) == 0);
  pollfd p{fd, POLLIN, 0};
  CHECK(poll(&p, 1, 1000) == 1 && (p.revents & POLLIN));
  CHECK(fcntl(fd, F_GETFD) & FD_CLOEXEC);
}

int main() {
  setvbuf(stdout, nullptr, _IONBF, 0);
  ABinderProcess_startThreadPool();
  const std::string name = std::string(IComposer::descriptor) + "/default";
  CHECK(AServiceManager_isDeclared(name.c_str()));
  auto composer = IComposer::fromBinder(
    ndk::SpAIBinder(AServiceManager_waitForService(name.c_str()))
  );
  if (!composer) {
    fprintf(stderr, "no composer\n");
    return 1;
  }
  int32_t ver = 0;
  composer->getInterfaceVersion(&ver);
  std::vector<Capability> caps;
  composer->getCapabilities(&caps);
  printf("IComposer V%d, %zu capabilities\n", ver, caps.size());
  CHECK(ver == 5);
  CHECK(
    std::find(
      caps.begin(), caps.end(), Capability::PRESENT_FENCE_IS_NOT_RELIABLE
    ) != caps.end()
  );  // GPU completion is not a host scanout timestamp.

  std::shared_ptr<IComposerClient> client;
  CHECK(composer->createClient(&client).isOk() && client);
  if (!client) return 1;
  auto cb = ndk::SharedRefBase::make<Callback>();
  CHECK(client->registerCallback(cb).isOk());
  CHECK(cb->hotplugs == 1);

  const int64_t D = 0;
  std::vector<DisplayConfiguration> configs;
  CHECK(
    client->getDisplayConfigurations(D, 0, &configs).isOk() &&
    configs.size() == 1
  );
  const auto& cfg = configs.at(0);
  printf(
    "display 0: config %d %dx%d vsync %d ns dpi %.0f\n", cfg.configId,
    cfg.width, cfg.height, cfg.vsyncPeriod, cfg.dpi ? cfg.dpi->x : 0.f
  );
  int32_t attr = 0;
  CHECK(
    client->getDisplayAttribute(D, cfg.configId, DisplayAttribute::WIDTH, &attr)
      .isOk() &&
    attr == cfg.width
  );
  CHECK(
    client->getDisplayAttribute(D, 99, DisplayAttribute::WIDTH, &attr)
      .getServiceSpecificError() == IComposerClient::EX_BAD_CONFIG
  );
  CHECK(
    client->setPowerMode(1234, PowerMode::ON).getServiceSpecificError() ==
    IComposerClient::EX_BAD_DISPLAY
  );
  CHECK(client->setPowerMode(D, PowerMode::ON).isOk());

  // Vsync: ~60 Hz while enabled, silent when disabled.
  CHECK(client->setVsyncEnabled(D, true).isOk());
  std::this_thread::sleep_for(std::chrono::milliseconds(1000));
  CHECK(client->setVsyncEnabled(D, false).isOk());
  {
    std::lock_guard l(cb->mutex);
    auto& s = cb->stamps;
    double avg =
      s.size() > 1 ? double(s.back() - s.front()) / (s.size() - 1) : 0;
    int64_t maxJitter = 0;
    for (size_t i = 1; i < s.size(); i++)
      maxJitter =
        std::max(maxJitter, std::abs((s[i] - s[i - 1]) - cfg.vsyncPeriod));
    printf(
      "vsync: %zu callbacks in 1s, mean interval %.3f ms, max deviation %.3f "
      "ms, "
      "period arg %d\n",
      s.size(), avg / 1e6, maxJitter / 1e6, cb->lastPeriod
    );
    CHECK(s.size() >= 55 && s.size() <= 62);
    CHECK(
      std::abs(avg - cfg.vsyncPeriod) < 100000
    );  // timestamps are on the grid
  }
  std::this_thread::sleep_for(std::chrono::milliseconds(50));
  int before = cb->vsyncs;
  std::this_thread::sleep_for(std::chrono::milliseconds(200));
  CHECK(cb->vsyncs == before);

  // Client target from our allocator, filled by the "renderer" (us).
  auto alloc = IAllocator::fromBinder(
    ndk::SpAIBinder(AServiceManager_waitForService(
      (std::string(IAllocator::descriptor) + "/default").c_str()
    ))
  );
  BufferDescriptorInfo d;
  d.name.fill(0);
  memcpy(d.name.data(), "FramebufferSurface", 18);
  d.width = cfg.width;
  d.height = cfg.height;
  d.layerCount = 1;
  d.format = PixelFormat::RGBA_8888;
  d.usage = BufferUsage(
    int64_t(BufferUsage::GPU_RENDER_TARGET) |
    int64_t(BufferUsage::COMPOSER_CLIENT_TARGET) |
    int64_t(BufferUsage::CPU_WRITE_OFTEN)
  );
  AllocationResult ar;
  CHECK(alloc && alloc->allocate2(d, 2, &ar).isOk());
  if (ar.buffers.size() != 2) return 1;
  auto fill = [&](const NativeHandle& h, uint32_t phase) {
    int fd = h.fds[0].get();
    linuxapk::BufferHandle bh;
    memcpy(&bh.magic, h.ints.data(), h.ints.size() * sizeof(int));
    uint8_t* p = (uint8_t*)mmap(
      nullptr, bh.totalSize.get(), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0
    );
    for (int y = 0; y < cfg.height; y++) {
      uint32_t* row = (uint32_t*)(p + size_t(y) * bh.planes[0].stride);
      for (int x = 0; x < cfg.width; x++) {
        uint8_t r = uint8_t(x * 255 / cfg.width),
                g = uint8_t(y * 255 / cfg.height);
        bool box = ((x / 80 + y / 80 + phase) % 2) == 0;
        row[x] = 0xff000000u | (box ? 0xc0u << 16 : 0) | g << 8 | r;
      }
    }
    munmap(p, bh.totalSize.get());
  };
  fill(ar.buffers[0], 0);
  fill(ar.buffers[1], 1);

  CHECK(client->setClientTargetSlotCount(D, 3).isOk());
  int64_t l1 = 0, l2 = 0;
  CHECK(client->createLayer(D, 3, &l1).isOk());
  CHECK(client->createLayer(D, 3, &l2).isOk());

  // Frame 1: SF asks for DEVICE + SOLID_COLOR; we must answer CLIENT for both.
  std::vector<DisplayCommand> cmds(1);
  cmds[0].display = D;
  LayerCommand lc1, lc2;
  lc1.layer = l1;
  lc1.composition = ParcelableComposition{Composition::DEVICE};
  lc1.z = ZOrder{0};
  lc1.displayFrame = Rect{0, 0, cfg.width, cfg.height};
  lc2.layer = l2;
  lc2.composition = ParcelableComposition{Composition::SOLID_COLOR};
  lc2.z = ZOrder{1};
  lc2.color = Color{1, 0, 0, 1};
  cmds[0].layers.push_back(std::move(lc1));
  cmds[0].layers.push_back(std::move(lc2));
  cmds[0].validateDisplay = true;
  std::vector<CommandResultPayload> res;
  CHECK(client->executeCommands(cmds, &res).isOk());
  CHECK(countErrors(res) == 0);
  auto* changed = find<CommandResultPayload::changedCompositionTypes>(res);
  CHECK(changed && changed->layers.size() == 2);
  if (changed) {
    for (auto& c : changed->layers) CHECK(c.composition == Composition::CLIENT);
    printf("validate: %zu layers changed to CLIENT\n", changed->layers.size());
  }

  auto present = [&](int slot, bool withHandle, bool accept) {
    DisplayCommand c;
    c.display = D;
    c.acceptDisplayChanges = accept;
    ClientTarget ct;
    ct.buffer.slot = slot;
    if (withHandle) ct.buffer.handle = dupHandle(ar.buffers[slot]);
    if (size_t(slot) < ar.buffers.size())
      ct.buffer.fence =
        ndk::ScopedFileDescriptor(producerFence(ar.buffers[slot]));
    ct.dataspace = Dataspace::SRGB;
    c.clientTarget = std::move(ct);
    c.presentDisplay = true;
    std::vector<DisplayCommand> v;
    v.push_back(std::move(c));
    std::vector<CommandResultPayload> r;
    CHECK(client->executeCommands(v, &r).isOk());
    return r;
  };
  int64_t t0 = nowNs();
  res = present(0, true, true);
  printf("present: %.2f ms\n", (nowNs() - t0) / 1e6);
  CHECK(countErrors(res) == 0);
  auto* rf = find<CommandResultPayload::releaseFences>(res);
  CHECK(rf && rf->display == D);
  CHECK(rf && rf->layers.empty());  // CLIENT layers are read by RenderEngine.
  const auto* pf = find<CommandResultPayload::presentFence>(res);
  CHECK(pf && pf->display == D);
  if (pf) {
    checkFence(pf->fence.get());
    int received = pf->fence.get();
    ndk::ScopedFileDescriptor duplicate(fcntl(received, F_DUPFD_CLOEXEC, 0));
    res.clear();
    CHECK(fcntl(received, F_GETFD) == -1);
    checkFence(duplicate.get());  // Closing one owner does not retire another.
  }

  // Present without validate -> NOT_VALIDATED.
  res = present(1, true, false);
  CHECK(countErrors(res) == 1);
  auto* e = find<CommandResultPayload::error>(res);
  CHECK(e && e->errorCode == IComposerClient::EX_NOT_VALIDATED);

  // presentOrValidate -> Validated; then frames alternating cached slots.
  for (int i = 0; i < 6; i++) {
    DisplayCommand c;
    c.display = D;
    c.presentOrValidateDisplay = true;
    std::vector<DisplayCommand> v;
    v.push_back(std::move(c));
    std::vector<CommandResultPayload> r;
    client->executeCommands(v, &r);
    auto* pov = find<CommandResultPayload::presentOrValidateResult>(r);
    CHECK(pov && pov->result == PresentOrValidate::Result::Validated);
    res = present(i % 2, i == 0 ? true : false, true);
    CHECK(countErrors(res) == 0);
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
  }
  // An empty slot with no handle is an error.
  {
    DisplayCommand c;
    c.display = D;
    c.validateDisplay = true;
    std::vector<DisplayCommand> v;
    v.push_back(std::move(c));
    std::vector<CommandResultPayload> r;
    client->executeCommands(v, &r);
  }
  res = present(2, false, true);
  CHECK(countErrors(res) == 1);

  // No fresh client target must not replay a prior present fence.
  {
    DisplayCommand c;
    c.display = D;
    c.validateDisplay = true;
    c.presentDisplay = true;
    std::vector<DisplayCommand> v;
    v.push_back(std::move(c));
    client->executeCommands(v, &res);
    CHECK(countErrors(res) == 0);
    CHECK(find<CommandResultPayload::presentFence>(res) == nullptr);
  }

  // CURSOR is the one layer type HWC reads. Return a real release sync_file
  // for that layer, while CLIENT layers continue to have no HWC release fd.
  {
    auto cursorDesc = d;
    cursorDesc.width = cursorDesc.height = 64;
    AllocationResult cursor;
    CHECK(alloc->allocate2(cursorDesc, 1, &cursor).isOk());
    if (cursor.buffers.size() == 1) {
      DisplayCommand c;
      c.display = D;
      LayerCommand layer;
      layer.layer = l2;
      layer.composition = ParcelableComposition{Composition::CURSOR};
      Buffer buffer;
      buffer.slot = 0;
      buffer.handle = dupHandle(cursor.buffers[0]);
      buffer.fence =
        ndk::ScopedFileDescriptor(producerFence(cursor.buffers[0]));
      layer.buffer = std::move(buffer);
      c.layers.push_back(std::move(layer));
      c.validateDisplay = true;
      c.presentDisplay = true;
      std::vector<DisplayCommand> v;
      v.push_back(std::move(c));
      client->executeCommands(v, &res);
      CHECK(countErrors(res) == 0);
      auto* released = find<CommandResultPayload::releaseFences>(res);
      CHECK(released && released->layers.size() == 1);
      if (released && released->layers.size() == 1) {
        CHECK(released->layers[0].layer == l2);
        checkFence(released->layers[0].fence.get());
      }
    }
  }

  // Virtual output completion is a real sync_file too, including the
  // dma-buf-export path when no explicit producer fence is supplied.
  {
    VirtualDisplay vd;
    CHECK(client
            ->createVirtualDisplay(
              cfg.width, cfg.height, PixelFormat::RGBA_8888, 2, &vd
            )
            .isOk());
    DisplayCommand c;
    c.display = vd.display;
    ClientTarget ct;
    ct.buffer.slot = 0;
    ct.buffer.handle = dupHandle(ar.buffers[0]);
    c.clientTarget = std::move(ct);
    Buffer output;
    output.slot = 0;
    output.handle = dupHandle(ar.buffers[0]);
    c.virtualDisplayOutputBuffer = std::move(output);
    c.validateDisplay = true;
    c.presentDisplay = true;
    std::vector<DisplayCommand> v;
    v.push_back(std::move(c));
    client->executeCommands(v, &res);
    CHECK(countErrors(res) == 0);
    auto* presented = find<CommandResultPayload::presentFence>(res);
    CHECK(presented && presented->display == vd.display);
    if (presented) checkFence(presented->fence.get());
    CHECK(client->destroyVirtualDisplay(vd.display).isOk());
    if (presented)
      checkFence(presented->fence.get());  // Binder fd outlives display.
  }

  // Dump check (set vendor.linuxapk.hwc.dump.dir for the composer).
  if (const char* dir = getenv("EXPECT_DUMP")) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1500));
    struct stat st;
    std::string f = std::string(dir) + "/display0-latest.png";
    bool ok = stat(f.c_str(), &st) == 0 && st.st_size > 1000;
    printf(
      "dump %s: %s (%lld bytes)\n", f.c_str(), ok ? "present" : "MISSING",
      ok ? (long long)st.st_size : 0LL
    );
    CHECK(ok);
  }

  CHECK(client->destroyLayer(D, l1).isOk());
  CHECK(
    client->destroyLayer(D, l1).getServiceSpecificError() ==
    IComposerClient::EX_BAD_LAYER
  );
  CHECK(client->setPowerMode(D, PowerMode::OFF).isOk());
  printf("%d checks, %d failures\n", gChecks, gFail);
  return gFail ? 1 : 0;
}
