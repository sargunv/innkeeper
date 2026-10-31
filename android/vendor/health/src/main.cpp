// SPDX-License-Identifier: AGPL-3.0-or-later
// android.hardware.health.IHealth/default (AIDL V5), fed by the bridge's
// host state service: the host's battery (UPower) instead of sysfs, which
// the session masks. Modeled on cuttlefish's health-aidl.cpp and the
// callback contract of hardware/interfaces/health/aidl/default: every
// registered IHealthInfoCallback gets the current HealthInfo at once and on
// each change.
//
// Health starts with the other HALs, before the bridge may be up: until the
// bridge answers it reports AC power and no battery, and it reconnects
// whenever the bridge goes away, as the composer does.
#define LOG_TAG "innkeeper-health"

#include <algorithm>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <aidl/android/hardware/health/BnHealth.h>
#include <aidl/android/hardware/health/IHealthInfoCallback.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>
#include <errno.h>
#include <pthread.h>
#include <string.h>
#include <unistd.h>

#include "battery.h"
#include "innkeeper/bridge_client.h"
#include "innkeeper/log.h"

using aidl::android::hardware::health::BatteryCapacityLevel;
using aidl::android::hardware::health::BatteryChargingPolicy;
using aidl::android::hardware::health::BatteryChargingState;
using aidl::android::hardware::health::BatteryHealth;
using aidl::android::hardware::health::BatteryHealthData;
using aidl::android::hardware::health::BatteryStatus;
using aidl::android::hardware::health::BnHealth;
using aidl::android::hardware::health::DiskStats;
using aidl::android::hardware::health::HealthInfo;
using aidl::android::hardware::health::HingeInfo;
using aidl::android::hardware::health::IHealthInfoCallback;
using aidl::android::hardware::health::StorageInfo;
using ndk::ScopedAStatus;

namespace {

ScopedAStatus unsupported() {
  return ScopedAStatus::fromExceptionCode(EX_UNSUPPORTED_OPERATION);
}

class Health : public BnHealth {
 public:
  Health() { set(INNKEEPER_BATTERY_AC_ONLY); }

  // A new state from the bridge: every callback hears it.
  void set(const innkeeper_battery& b) {
    innkeeper_health h;
    innkeeper_health_from(&b, &h);
    HealthInfo i;
    i.chargerAcOnline = h.ac_online;
    i.batteryPresent = h.battery_present;
    i.batteryStatus = BatteryStatus(h.status);
    i.batteryHealth =
      h.battery_present ? BatteryHealth::GOOD : BatteryHealth::UNKNOWN;
    i.batteryLevel = h.level;
    i.batteryCapacityLevel = BatteryCapacityLevel(h.capacity_level);
    i.batteryChargeTimeToFullNowSeconds = h.time_to_full_s;
    i.chargingState = BatteryChargingState::NORMAL;
    i.chargingPolicy = BatteryChargingPolicy::DEFAULT;
    // A room-temperature battery: BatteryService shuts down when hot.
    i.batteryTemperatureTenthsCelsius = 250;
    i.batteryVoltageMillivolts = h.battery_present ? 3800 : 0;
    i.batteryTechnology = h.battery_present ? "Li-ion" : "";
    std::vector<std::shared_ptr<IHealthInfoCallback>> cbs;
    {
      std::lock_guard lock(mMutex);
      mInfo = i;
      cbs = mCallbacks;
    }
    LOGI(
      "battery: present %d, level %d, status %d, ac %d", i.batteryPresent,
      i.batteryLevel, int(i.batteryStatus), i.chargerAcOnline
    );
    notify(cbs, i);
  }

  ScopedAStatus registerCallback(
    const std::shared_ptr<IHealthInfoCallback>& cb
  ) override {
    if (!cb) return ScopedAStatus::fromExceptionCode(EX_NULL_POINTER);
    HealthInfo i;
    {
      std::lock_guard lock(mMutex);
      mCallbacks.push_back(cb);
      i = mInfo;
    }
    // The default implementation's contract: the current state at once.
    cb->healthInfoChanged(i);
    return ScopedAStatus::ok();
  }

  ScopedAStatus unregisterCallback(
    const std::shared_ptr<IHealthInfoCallback>& cb
  ) override {
    std::lock_guard lock(mMutex);
    auto it = std::find_if(mCallbacks.begin(), mCallbacks.end(), [&](auto& c) {
      return c->asBinder() == cb->asBinder();
    });
    if (it == mCallbacks.end())
      return ScopedAStatus::fromExceptionCode(EX_ILLEGAL_ARGUMENT);
    mCallbacks.erase(it);
    return ScopedAStatus::ok();
  }

  ScopedAStatus update() override {
    std::vector<std::shared_ptr<IHealthInfoCallback>> cbs;
    HealthInfo i;
    {
      std::lock_guard lock(mMutex);
      cbs = mCallbacks;
      i = mInfo;
    }
    notify(cbs, i);
    return ScopedAStatus::ok();
  }

  ScopedAStatus getHealthInfo(HealthInfo* out) override {
    std::lock_guard lock(mMutex);
    *out = mInfo;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getCapacity(int32_t* out) override {
    std::lock_guard lock(mMutex);
    *out = mInfo.batteryLevel;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getChargeStatus(BatteryStatus* out) override {
    std::lock_guard lock(mMutex);
    *out = mInfo.batteryStatus;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getChargeCounterUah(int32_t*) override { return unsupported(); }
  ScopedAStatus getCurrentNowMicroamps(int32_t*) override {
    return unsupported();
  }
  ScopedAStatus getCurrentAverageMicroamps(int32_t*) override {
    return unsupported();
  }
  ScopedAStatus getEnergyCounterNwh(int64_t*) override { return unsupported(); }
  ScopedAStatus getStorageInfo(std::vector<StorageInfo>*) override {
    return unsupported();
  }
  ScopedAStatus getDiskStats(std::vector<DiskStats>*) override {
    return unsupported();
  }
  ScopedAStatus setChargingPolicy(BatteryChargingPolicy) override {
    return unsupported();
  }
  ScopedAStatus getChargingPolicy(BatteryChargingPolicy*) override {
    return unsupported();
  }
  ScopedAStatus getBatteryHealthData(BatteryHealthData*) override {
    return unsupported();
  }
  ScopedAStatus getHingeInfo(std::vector<HingeInfo>*) override {
    return unsupported();
  }

 private:
  // Outside the lock: a callback into system_server may take a while, and a
  // dead one is dropped.
  void notify(
    const std::vector<std::shared_ptr<IHealthInfoCallback>>& cbs,
    const HealthInfo& i
  ) {
    for (auto& cb : cbs) {
      if (!cb->healthInfoChanged(i).isOk()) {
        std::lock_guard lock(mMutex);
        std::erase(mCallbacks, cb);
      }
    }
  }

  std::mutex mMutex;
  HealthInfo mInfo;
  std::vector<std::shared_ptr<IHealthInfoCallback>> mCallbacks;
};

// The bridge connection: START (battery), then BATTERY events, reconnecting
// once a second while the bridge is missing.
void bridgeLoop(std::shared_ptr<Health> health) {
  pthread_setname_np(pthread_self(), "health-bridge");
  lxb_backoff backoff{};
  int lastError = 0;
  while (true) {
    if (const int ms = lxb_backoff_ms(&backoff); ms > 0) {
      usleep(useconds_t(ms) * 1000);
      continue;
    }
    const int s = lxb_connect(LXB_SERVICE_HOSTSTATE, "health HAL");
    if (s < 0) {
      if (s != lastError)
        LOGW("bridge: connect: %s; retrying every second", strerror(-s));
      lastError = s;
      continue;
    }
    lastError = 0;
    lxb_msg m;
    lxb_msg_init(&m, INNKEEPER_HOSTSTATE_START, 2);
    lxb_put_u32(&m, INNKEEPER_FEATURE_BATTERY);
    int r = lxb_send(s, &m, -1);
    // The current state comes as an event, possibly before START's reply.
    while (r == 0) {
      lxb_packet p;
      r = lxb_recv(s, &p);
      if (r != 0) break;
      if (p.type == LXB_REPLY && p.status != 0)
        LOGW("bridge: START refused: %d", p.status);
      innkeeper_battery b;
      if (p.type == INNKEEPER_EV_BATTERY) {
        if (innkeeper_battery_decode(p.body, p.len, &b) == 0)
          health->set(b);
        else
          LOGW("bridge: malformed BATTERY event (%zu bytes)", p.len);
      }
      lxb_close_fds(&p);
    }
    LOGW("bridge: connection lost (%s); reconnecting", strerror(-r));
    close(s);
  }
}

}  // namespace

int main() {
  ABinderProcess_setThreadPoolMaxThreadCount(2);
  ABinderProcess_startThreadPool();
  auto health = ndk::SharedRefBase::make<Health>();
  std::thread(bridgeLoop, health).detach();
  const std::string instance = std::string(Health::descriptor) + "/default";
  if (
    AServiceManager_addService(health->asBinder().get(), instance.c_str()) !=
    STATUS_OK
  ) {
    LOGE("failed to register %s", instance.c_str());
    return 1;
  }
  LOGI("registered %s (V%d)", instance.c_str(), Health::version);
  ABinderProcess_joinThreadPool();
  return 1;
}
