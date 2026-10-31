// SPDX-License-Identifier: AGPL-3.0-only
#include <algorithm>
#include <vector>

#include <android/log.h>

#include "StreamBridge.h"

#include "Output.h"

// Declared in tinyalsa/pcm.c. Not part of the image's tinyalsa header: it
// reports this playback stream's Android audio_usage_t to the bridge.
extern "C" void pcm_set_usage(struct pcm* pcm, unsigned int usage);

namespace linuxapk::audio {
StreamBridge::StreamBridge(size_t frames)
    : IStreamCommonDelegator(
        ndk::SharedRefBase::make<
          Defaults<core::IStreamCommonDefault, core::IStreamCommon>>()
      ),
      data_(frames * kFrameBytes) {
  worker_ = std::thread([this] { run(); });
}
StreamBridge::~StreamBridge() { close(); }
void StreamBridge::describe(Descriptor* out) {
  out->command = command_.descriptor();
  out->reply = reply_.descriptor();
  out->frameSizeBytes = kFrameBytes;
  out->bufferSizeFrames = data_.size() / kFrameBytes;
  out->audio.set<Descriptor::AudioBuffer::Tag::fmq>(data_.descriptor());
}
ndk::ScopedAStatus StreamBridge::close() {
  std::lock_guard lock(closeMutex_);
  closed_ = true;
  command_.wake(2);
  if (worker_.joinable()) worker_.join();
  return ndk::ScopedAStatus::ok();
}
void StreamBridge::publishUsage() {
  if (pcm_ && pendingUsage_ && reportedUsage_ != pendingUsage_) {
    pcm_set_usage(pcm_, static_cast<unsigned>(*pendingUsage_));
    reportedUsage_ = pendingUsage_;
  }
}
void StreamBridge::noteUsage(std::optional<int32_t> usage) {
  if (!usage) return;
  std::lock_guard lock(closeMutex_);
  pendingUsage_ = usage;
  publishUsage();
}
bool StreamBridge::open() {
  if (pcm_) return true;
  pcm_config config{};
  config.channels = kChannels;
  config.rate = kRate;
  config.period_size = kPeriodFrames;
  config.period_count = kPeriods;
  config.format = PCM_FORMAT_S16_LE;
  pcm_ = pcm_open(0, 0, PCM_OUT, &config);
  if (!pcm_is_ready(pcm_)) {
    stop();
    return false;
  }
  // Caller holds closeMutex_. A usage can arrive before the ring exists.
  publishUsage();
  return true;
}
void StreamBridge::stop() {
  if (pcm_) {
    pcm_close(pcm_);
    pcm_ = nullptr;
    reportedUsage_ = std::nullopt;
  }
}
void StreamBridge::position(Descriptor::Reply* reply) {
  timespec now{};
  clock_gettime(CLOCK_MONOTONIC, &now);
  uint64_t queued = pcm_ ? std::max(0L, pcm_get_delay(pcm_)) : 0;
  reply->observable.frames = presentedFrames(written_, queued);
  reply->observable.timeNs = now.tv_sec * 1000000000LL + now.tv_nsec;
  reply->latencyMs = latencyMs(queued);
  reply->state = state_;
}
ndk::ScopedAStatus StreamOut::updateMetadata(
  const aidl::android::hardware::audio::common::SourceMetadata& metadata
) {
  std::vector<int32_t> usages;
  usages.reserve(metadata.tracks.size());
  for (const auto& track : metadata.tracks) {
    usages.push_back(static_cast<int32_t>(track.usage));
  }
  // Empty track list: leave the last label in place. A repeated usage is
  // not resent; noteUsage applies that once the bridge stream exists.
  stream_->noteUsage(selectUsage(usages.data(), usages.size()));
  return ndk::ScopedAStatus::ok();
}

void StreamBridge::run() {
  std::vector<int8_t> samples(data_.size());
  Descriptor::Command command;
  while (command_.readBlocking(&command, closed_)) {
    Descriptor::Reply reply;
    using Tag = Descriptor::Command::Tag;
    switch (command.getTag()) {
      case Tag::getStatus:
        break;
      case Tag::start: {
        std::lock_guard lock(closeMutex_);
        if (!open()) {
          reply.status = -ENODEV;
          break;
        }
        state_ = state_ == Descriptor::State::PAUSED ? Descriptor::State::ACTIVE
                                                     : Descriptor::State::IDLE;
        break;
      }
      case Tag::burst: {
        auto bytes = command.get<Tag::burst>();
        bool opened = false;
        {
          // Don't hold this across pcm_write: it blocks on the ring, and
          // updateMetadata needs the lock to publish a usage change.
          std::lock_guard lock(closeMutex_);
          opened = open();
        }
        if (
          !opened || bytes < 0 || bytes % kFrameBytes ||
          static_cast<size_t>(bytes) > samples.size() ||
          !data_.read(samples.data(), bytes)
        ) {
          reply.status = -EINVAL;
          break;
        }
        int status = pcm_write(pcm_, samples.data(), bytes);
        if (status < 0) {
          reply.status = status;
          break;
        }
        written_ += bytes / kFrameBytes;
        reply.fmqByteCount = bytes;
        state_ = Descriptor::State::ACTIVE;
        break;
      }
      case Tag::drain:
        if (pcm_) pcm_drain(pcm_);
        state_ = Descriptor::State::IDLE;
        break;
      case Tag::standby:
        stop();
        state_ = Descriptor::State::STANDBY;
        break;
      case Tag::pause:
        if (pcm_) pcm_stop(pcm_);
        state_ = Descriptor::State::PAUSED;
        break;
      case Tag::flush:
        stop();
        state_ = Descriptor::State::IDLE;
        break;
      default:
        reply.status = -ENOSYS;
        break;
    }
    position(&reply);
    if (!reply_.write(reply)) break;
  }
  stop();
}
}  // namespace linuxapk::audio
