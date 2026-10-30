// SPDX-License-Identifier: AGPL-3.0-only
#include <algorithm>
#include <map>

#include <aidl/android/hardware/audio/core/AudioPatch.h>
#include <aidl/android/hardware/audio/core/AudioRoute.h>
#include <aidl/android/hardware/audio/core/BnModule.h>
#include <aidl/android/media/audio/common/AudioDeviceType.h>
#include <aidl/android/media/audio/common/AudioOutputFlags.h>
#include <android/binder_manager.h>
#include <android/binder_process.h>

#include "Output.h"
#include "StreamBridge.h"

namespace linuxapk::audio {
namespace common = aidl::android::media::audio::common;
using common::AudioPort;
using common::AudioPortConfig;
using ndk::ScopedAStatus;

class Module : public core::IModuleDelegator {
 public:
  Module()
      : IModuleDelegator(
          ndk::SharedRefBase::make<
            Defaults<core::IModuleDefault, core::IModule>>()
        ) {
    common::AudioProfile profile;
    profile.format.type = common::AudioFormatType::PCM;
    profile.format.pcm = common::PcmType::INT_16_BIT;
    profile.sampleRates = {48000};
    profile.channelMasks = {common::AudioChannelLayout::make<
      common::AudioChannelLayout::Tag::layoutMask>(
      common::AudioChannelLayout::LAYOUT_STEREO
    )};
    AudioPort mix;
    mix.id = 1;
    mix.name = "primary output";
    mix.profiles = {profile};
    mix.flags = common::AudioIoFlags::make<common::AudioIoFlags::Tag::output>(
      (1 << static_cast<int>(common::AudioOutputFlags::PRIMARY)) |
      (1 << static_cast<int>(common::AudioOutputFlags::FAST))
    );
    common::AudioPortMixExt mixExt;
    mixExt.maxOpenStreamCount = 1;
    mixExt.maxActiveStreamCount = 1;
    mix.ext =
      common::AudioPortExt::make<common::AudioPortExt::Tag::mix>(mixExt);
    AudioPort speaker;
    speaker.id = 2;
    speaker.name = "Speaker";
    speaker.profiles = {profile};
    speaker.flags =
      common::AudioIoFlags::make<common::AudioIoFlags::Tag::output>(0);
    common::AudioPortDeviceExt device;
    device.device.type.type = common::AudioDeviceType::OUT_SPEAKER;
    device.flags = 1 << common::AudioPortDeviceExt::FLAG_INDEX_DEFAULT_DEVICE;
    speaker.ext =
      common::AudioPortExt::make<common::AudioPortExt::Tag::device>(device);
    ports_ = {mix, speaker};
    core::AudioRoute route;
    route.sinkPortId = 2;
    route.sourcePortIds = {1};
    routes_ = {route};
  }
  ScopedAStatus getAudioPorts(std::vector<AudioPort>* out) override {
    *out = ports_;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getAudioPort(int32_t id, AudioPort* out) override {
    if (id < 1 || id > 2) return invalid();
    *out = ports_[id - 1];
    return ScopedAStatus::ok();
  }
  ScopedAStatus getAudioRoutes(std::vector<core::AudioRoute>* out) override {
    *out = routes_;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getAudioRoutesForAudioPort(
    int32_t id, std::vector<core::AudioRoute>* out
  ) override {
    if (id < 1 || id > 2) return invalid();
    return getAudioRoutes(out);
  }
  ScopedAStatus getAudioPortConfigs(
    std::vector<AudioPortConfig>* out
  ) override {
    std::lock_guard lock(mutex_);
    out->clear();
    for (const auto& [id, c] : configs_) out->push_back(c);
    return ScopedAStatus::ok();
  }
  ScopedAStatus setAudioPortConfig(
    const AudioPortConfig& requested, AudioPortConfig* suggested, bool* applied
  ) override {
    std::lock_guard lock(mutex_);
    if (requested.portId < 1 || requested.portId > 2) return invalid();
    *suggested = requested;
    auto& profile = ports_[requested.portId - 1].profiles[0];
    suggested->sampleRate = common::Int{.value = 48000};
    suggested->format = profile.format;
    suggested->channelMask = profile.channelMasks[0];
    suggested->flags = ports_[requested.portId - 1].flags;
    if (
      !requested.sampleRate || !requested.format || !requested.channelMask ||
      requested.sampleRate->value != 48000 ||
      requested.format != suggested->format ||
      requested.channelMask != suggested->channelMask
    ) {
      *applied = false;
      return ScopedAStatus::ok();
    }
    if (requested.id && !configs_.contains(requested.id)) return invalid();
    if (!suggested->id) suggested->id = nextConfig_++;
    configs_[suggested->id] = *suggested;
    *applied = true;
    return ScopedAStatus::ok();
  }
  ScopedAStatus resetAudioPortConfig(int32_t id) override {
    std::lock_guard lock(mutex_);
    if (!configs_.erase(id)) return invalid();
    return ScopedAStatus::ok();
  }
  ScopedAStatus getAudioPatches(std::vector<core::AudioPatch>* out) override {
    std::lock_guard lock(mutex_);
    out->clear();
    for (const auto& [id, p] : patches_) out->push_back(p);
    return ScopedAStatus::ok();
  }
  ScopedAStatus setAudioPatch(
    const core::AudioPatch& requested, core::AudioPatch* out
  ) override {
    std::lock_guard lock(mutex_);
    if (
      requested.sourcePortConfigIds.size() != 1 ||
      requested.sinkPortConfigIds.size() != 1
    )
      return invalid();
    auto source = configs_.find(requested.sourcePortConfigIds[0]),
         sink = configs_.find(requested.sinkPortConfigIds[0]);
    if (
      source == configs_.end() || sink == configs_.end() ||
      source->second.portId != 1 || sink->second.portId != 2
    )
      return invalid();
    if (requested.id && !patches_.contains(requested.id)) return invalid();
    *out = requested;
    if (!out->id) out->id = nextPatch_++;
    out->minimumStreamBufferSizeFrames = kPeriodFrames;
    out->latenciesMs = {kNominalLatencyMs};
    patches_[out->id] = *out;
    return ScopedAStatus::ok();
  }
  ScopedAStatus resetAudioPatch(int32_t id) override {
    std::lock_guard lock(mutex_);
    if (!patches_.erase(id)) return invalid();
    return ScopedAStatus::ok();
  }
  ScopedAStatus openOutputStream(
    const core::IModule::OpenOutputStreamArguments& args,
    core::IModule::OpenOutputStreamReturn* out
  ) override {
    std::lock_guard lock(mutex_);
    auto c = configs_.find(args.portConfigId);
    if (
      c == configs_.end() || c->second.portId != 1 ||
      !validBufferSize(args.bufferSizeFrames) || args.offloadInfo ||
      args.callback
    )
      return invalid();
    auto stream = ndk::SharedRefBase::make<StreamBridge>(args.bufferSizeFrames);
    stream->describe(&out->desc);
    out->stream = ndk::SharedRefBase::make<StreamOut>(stream);
    return ScopedAStatus::ok();
  }
  ScopedAStatus getSupportedPlaybackRateFactors(
    core::IModule::SupportedPlaybackRateFactors* out
  ) override {
    out->minSpeed = out->maxSpeed = out->minPitch = out->maxPitch = 1;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getMicrophones(
    std::vector<common::MicrophoneInfo>* out
  ) override {
    out->clear();
    return ScopedAStatus::ok();
  }
  ScopedAStatus getMmapPolicyInfos(
    common::AudioMMapPolicyType, std::vector<common::AudioMMapPolicyInfo>* out
  ) override {
    out->clear();
    return ScopedAStatus::ok();
  }
  ScopedAStatus supportsVariableLatency(bool* out) override {
    *out = false;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getAAudioMixerBurstCount(int32_t* out) override {
    *out = 2;
    return ScopedAStatus::ok();
  }
  ScopedAStatus getAAudioHardwareBurstMinUsec(int32_t* out) override {
    *out = 1000;
    return ScopedAStatus::ok();
  }

 private:
  std::mutex mutex_;
  std::vector<AudioPort> ports_;
  std::vector<core::AudioRoute> routes_;
  std::map<int32_t, AudioPortConfig> configs_;
  std::map<int32_t, core::AudioPatch> patches_;
  int32_t nextConfig_ = 3, nextPatch_ = 1;
};
}  // namespace linuxapk::audio
int main() {
  ABinderProcess_setThreadPoolMaxThreadCount(4);
  ABinderProcess_startThreadPool();
  auto module = ndk::SharedRefBase::make<linuxapk::audio::Module>();
  if (
    AServiceManager_addService(
      module->asBinder().get(), "android.hardware.audio.core.IModule/linuxapk"
    ) != STATUS_OK
  )
    return 1;
  ABinderProcess_joinThreadPool();
  return 1;
}
