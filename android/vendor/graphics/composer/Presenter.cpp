// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-hwc"
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <tuple>

#include "Presenter.h"

#include "BridgePresenter.h"
#include "Display.h"
#include "Properties.h"
#include "innkeeper/log.h"

namespace innkeeper::hwc {

void HeadlessPresenter::present(const FrameInput& frame) {
  mPresented++;
  const int64_t now = nowNs();
  if (now - mLastLogNs > 60'000'000'000) {
    LOGI(
      "display %lld: %llu frames presented (headless)",
      (long long)frame.displayId, (unsigned long long)mPresented
    );
    mLastLogNs = now;
  }
}

void startPresenterBackend() {
  const std::string backend =
    getProperty("vendor.innkeeper.hwc.backend", "headless");
  if (backend == "bridge") {
    startBridge();
  } else if (backend != "headless") {
    LOGW("unknown backend '%s'; using headless", backend.c_str());
  }
}

std::unique_ptr<Presenter> createPrimaryPresenter(const DisplayConfig& config) {
  if (bridgeStarted() && getProperty("vendor.innkeeper.mode", "") == "phone")
    return createBridgePrimaryPresenter(
      uint32_t(config.width), uint32_t(config.height)
    );
  return std::make_unique<HeadlessPresenter>();
}

std::optional<std::pair<int32_t, int32_t>> primarySize() {
  int w = 0, h = 0;
  char end = 0;
  const std::string size = getProperty("vendor.innkeeper.hwc.primary", "");
  if (
    sscanf(size.c_str(), "%dx%d%c", &w, &h, &end) == 2 && w > 0 && h > 0 &&
    w <= 8192 && h <= 8192
  )
    return std::pair{w, h};
  return std::nullopt;
}

DisplayConfig primaryConfig() {
  DisplayConfig config;
  if (const auto size = primarySize()) {
    std::tie(config.width, config.height) = *size;
    config.dpi =
      std::max(1, atoi(getProperty("ro.sf.lcd_density", "160").c_str()));
  }
  return config;
}

std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
) {
  if (bridgeStarted())
    return createBridgeVirtualPresenter(displayId, width, height);
  return std::make_unique<HeadlessPresenter>();
}

}  // namespace innkeeper::hwc
