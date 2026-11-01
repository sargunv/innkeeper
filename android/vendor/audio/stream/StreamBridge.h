// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once
#include <mutex>
#include <optional>
#include <thread>

#include <aidl/android/hardware/audio/core/BnStreamCommon.h>
#include <aidl/android/hardware/audio/core/BnStreamOut.h>
#include <aidl/android/hardware/audio/core/StreamDescriptor.h>

#include "Defaults.h"
#include "Queue.h"
#include "Usage.h"
#include "innkeeper/pcm.h"

namespace innkeeper::audio {
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

  // Highest-priority usage among the active tracks, or nothing when the
  // track list is empty, which keeps the last label.
  void noteUsage(std::optional<int32_t> usage);

 private:
  void run();
  void stop();
  bool open();
  void publishUsage();
  void position(Descriptor::Reply* reply);
  Queue<Descriptor::Command> command_{1};
  Queue<Descriptor::Reply> reply_{1};
  Queue<int8_t> data_;
  std::atomic<bool> closed_{false};
  std::mutex closeMutex_;
  std::thread worker_;
  bridge_pcm* pcm_ = nullptr;
  uint64_t written_ = 0;
  Descriptor::State state_ = Descriptor::State::STANDBY;
  // Usage to forward once the bridge stream exists. Written by
  // updateMetadata, read by the worker under closeMutex_. bridge_pcm_set_usage
  // drops a repeat, including across a reconnect.
  std::optional<int32_t> pendingUsage_;
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
    const aidl::android::hardware::audio::common::SourceMetadata& metadata
  ) override;

 private:
  std::shared_ptr<StreamBridge> stream_;
};
}  // namespace innkeeper::audio
