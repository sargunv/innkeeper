// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include <mutex>
#include <thread>

#include <aidl/android/hardware/audio/core/BnStreamCommon.h>
#include <aidl/android/hardware/audio/core/BnStreamOut.h>
#include <aidl/android/hardware/audio/core/StreamDescriptor.h>
#include <tinyalsa/pcm.h>

#include "Defaults.h"
#include "Queue.h"

namespace linuxapk::audio {
namespace core = aidl::android::hardware::audio::core;
using Descriptor = core::StreamDescriptor;

class StreamBridge : public core::IStreamCommonDelegator {
 public:
  explicit StreamBridge(size_t frames);
  ~StreamBridge() override;
  void describe(Descriptor* result);
  ndk::ScopedAStatus close() override;
  ndk::ScopedAStatus prepareToClose() override {
    return ndk::ScopedAStatus::ok();
  }

 private:
  void run();
  void stop();
  bool open();
  void position(Descriptor::Reply* reply);
  Queue<Descriptor::Command> command_{1};
  Queue<Descriptor::Reply> reply_{1};
  Queue<int8_t> data_;
  std::atomic<bool> closed_{false};
  std::mutex closeMutex_;
  std::thread worker_;
  pcm* pcm_ = nullptr;
  uint64_t written_ = 0;
  Descriptor::State state_ = Descriptor::State::STANDBY;
};

class StreamOut : public core::IStreamOutDelegator {
 public:
  explicit StreamOut(std::shared_ptr<StreamBridge> stream)
      : IStreamOutDelegator(
          ndk::SharedRefBase::make<
            Defaults<core::IStreamOutDefault, core::IStreamOut>>()
        ),
        stream_(std::move(stream)) {}
  ndk::ScopedAStatus getStreamCommon(
    std::shared_ptr<core::IStreamCommon>* out
  ) override {
    *out = stream_;
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus updateMetadata(
    const aidl::android::hardware::audio::common::SourceMetadata&
  ) override {
    return ndk::ScopedAStatus::ok();
  }

 private:
  std::shared_ptr<StreamBridge> stream_;
};
}  // namespace linuxapk::audio
