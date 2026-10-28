// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include <string>

#include "Presenter.h"

#include "BridgePresenter.h"
#include "FrameDumper.h"
#include "Properties.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

namespace {
std::unique_ptr<HeadlessPresenter> makeHeadless() {
  return std::make_unique<HeadlessPresenter>(
    std::make_unique<FrameDumper>(FrameDumper::dirFromProperty())
  );
}
}  // namespace

void startPresenterBackend() {
  const std::string backend =
    getProperty("vendor.linuxapk.hwc.backend", "headless");
  if (backend == "bridge") {
    startBridge();
  } else if (backend != "headless") {
    LOGW("unknown backend '%s'; using headless", backend.c_str());
  }
}

std::unique_ptr<Presenter> createPrimaryPresenter() { return makeHeadless(); }

std::unique_ptr<Presenter> createVirtualPresenter(
  int64_t displayId, uint32_t width, uint32_t height
) {
  if (bridgeStarted())
    return createBridgeVirtualPresenter(
      displayId, width, height, makeHeadless()
    );
  return makeHeadless();
}

}  // namespace linuxapk::hwc
