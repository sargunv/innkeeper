// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include <aidl/android/hardware/audio/core/BnStreamIn.h>

#include "StreamBridge.h"

namespace innkeeper::audio {
class StreamInBridge : public core::IStreamCommonDelegator {
 public:
  StreamInBridge(
    size_t frames, uint32_t channels, std::shared_ptr<std::atomic<bool>> muted
  );
  ~StreamInBridge() override;
  void describe(Descriptor* result);
  ndk::ScopedAStatus close() override;
  ndk::ScopedAStatus prepareToClose() override {
    return ndk::ScopedAStatus::ok();
  }

 private:
  void run();
  bool open();
  void stop();
  Queue<Descriptor::Command> command_{1};
  Queue<Descriptor::Reply> reply_{1};
  Queue<int8_t> data_;
  const uint32_t channels_, frameBytes_;
  std::shared_ptr<std::atomic<bool>> muted_;
  std::atomic<bool> closed_{false};
  std::mutex closeMutex_;
  std::thread worker_;
  pcm* pcm_ = nullptr;
  uint64_t read_ = 0;
  Descriptor::State state_ = Descriptor::State::STANDBY;
};

class StreamIn : public core::IStreamInDelegator {
 public:
  StreamIn(std::shared_ptr<StreamInBridge> stream, uint32_t channels)
      : IStreamInDelegator(
          ndk::SharedRefBase::make<
            Defaults<core::IStreamInDefault, core::IStreamIn>>()
        ),
        stream_(std::move(stream)),
        channels_(channels) {}
  ndk::ScopedAStatus getStreamCommon(
    std::shared_ptr<core::IStreamCommon>* out
  ) override {
    *out = stream_;
    return ndk::ScopedAStatus::ok();
  }
  // Capture has no usage to report. The bridge drops SET_METADATA on record
  // streams (PulseOut::control returns before applying Ctl::Metadata), so
  // this stays a no-op.
  ndk::ScopedAStatus updateMetadata(
    const aidl::android::hardware::audio::common::SinkMetadata&
  ) override {
    return ndk::ScopedAStatus::ok();
  }
  ndk::ScopedAStatus getActiveMicrophones(
    std::vector<aidl::android::media::audio::common::MicrophoneDynamicInfo>* out
  ) override {
    using Info = aidl::android::media::audio::common::MicrophoneDynamicInfo;
    Info info;
    info.id = "innkeeper-microphone";
    info.channelMapping.assign(channels_, Info::ChannelMapping::DIRECT);
    *out = {info};
    return ndk::ScopedAStatus::ok();
  }

 private:
  std::shared_ptr<StreamInBridge> stream_;
  uint32_t channels_;
};
}  // namespace innkeeper::audio
