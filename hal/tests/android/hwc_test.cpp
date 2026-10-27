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
#include <atomic>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <string.h>
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
  CHECK(
    find<CommandResultPayload::presentFence>(res) == nullptr
  );  // headless: none

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

  // Dump check (set VENDOR_LINUXAPK_HWC_DUMP_DIR for the composer).
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
