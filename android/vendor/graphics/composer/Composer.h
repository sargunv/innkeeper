// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <condition_variable>
#include <memory>
#include <mutex>

#include <aidl/android/hardware/graphics/composer3/BnComposer.h>

namespace innkeeper::hwc {

class ComposerClient;

class Composer
    : public ::aidl::android::hardware::graphics::composer3::BnComposer {
 public:
  ndk::ScopedAStatus createClient(
    std::shared_ptr<
      ::aidl::android::hardware::graphics::composer3::IComposerClient>* out
  ) override;
  ndk::ScopedAStatus getCapabilities(
    std::vector<::aidl::android::hardware::graphics::composer3::Capability>* out
  ) override;

 protected:
  ndk::SpAIBinder createBinder() override;

 private:
  std::mutex mMutex;
  std::condition_variable mCv;
  bool mClientAlive = false;
};

}  // namespace innkeeper::hwc
