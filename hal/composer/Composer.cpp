// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "linuxapk-hwc"
#include "Composer.h"

#include <android/binder_ibinder_platform.h>

#include <chrono>

#include "Common.h"
#include "ComposerClient.h"
#include "linuxapk/log.h"

namespace linuxapk::hwc {

using namespace ::aidl::android::hardware::graphics::composer3;

ndk::ScopedAStatus Composer::createClient(std::shared_ptr<IComposerClient>* out) {
    std::unique_lock lock(mMutex);
    // A restarted SurfaceFlinger may connect before the old client is gone.
    if (!mCv.wait_for(lock, std::chrono::seconds(5), [this] { return !mClientAlive; })) {
        LOGE("createClient: previous client still alive");
        return toStatus(Error::NoResources);
    }
    auto client = ndk::SharedRefBase::make<ComposerClient>();
    client->setOnDestroyed([this] {
        std::lock_guard l(mMutex);
        mClientAlive = false;
        mCv.notify_all();
    });
    mClientAlive = true;
    *out = client;
    LOGI("composer client created");
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Composer::getCapabilities(std::vector<Capability>* out) {
    // We return no present fences (or unreliable ones from future backends).
    *out = {Capability::PRESENT_FENCE_IS_NOT_RELIABLE};
    return ndk::ScopedAStatus::ok();
}

ndk::SpAIBinder Composer::createBinder() {
    auto binder = BnComposer::createBinder();
    AIBinder_setInheritRt(binder.get(), true);
    return binder;
}

}  // namespace linuxapk::hwc
