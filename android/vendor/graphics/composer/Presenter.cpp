// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-hwc"
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <string>

#include "Presenter.h"

#include "BridgePresenter.h"
#include "Display.h"
#include "FrameDumper.h"
#include "Properties.h"
#include "innkeeper/log.h"

namespace innkeeper::hwc {

namespace {
std::unique_ptr<HeadlessPresenter> makeHeadless() {
  return std::make_unique<HeadlessPresenter>(
    std::make_unique<FrameDumper>(FrameDumper::dirFromProperty())
  );
}
}  // namespace

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
      uint32_t(config.width), uint32_t(config.height), makeHeadless()
    );
  return makeHeadless();
}

DisplayConfig primaryConfig() {
  DisplayConfig config;
  int w = 0, h = 0;
  const std::string size = getProperty("vendor.innkeeper.hwc.primary", "");
  if (
    sscanf(size.c_str(), "%dx%d", &w, &h) == 2 && w > 0 && h > 0 && w <= 8192 &&
    h <= 8192
  ) {
    config.width = w;
    config.height = h;
    config.dpi =
      std::max(1, atoi(getProperty("ro.sf.lcd_density", "160").c_str()));
  }
  return config;
}

std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
) {
  if (bridgeStarted())
    return createBridgeVirtualPresenter(
      displayId, width, height, makeHeadless()
    );
  return makeHeadless();
}

}  // namespace innkeeper::hwc
