// SPDX-License-Identifier: AGPL-3.0-or-later
#include <vector>

#include "StreamInBridge.h"

#include "Input.h"

namespace innkeeper::audio {
StreamInBridge::StreamInBridge(
  size_t frames, uint32_t channels, std::shared_ptr<std::atomic<bool>> muted
)
    : IStreamCommonDelegator(
        ndk::SharedRefBase::make<
          Defaults<core::IStreamCommonDefault, core::IStreamCommon>>()
      ),
      data_(frames * channels * sizeof(int16_t)),
      channels_(channels),
      frameBytes_(channels * sizeof(int16_t)),
      muted_(std::move(muted)) {
  worker_ = std::thread([this] { run(); });
}
StreamInBridge::~StreamInBridge() { close(); }
void StreamInBridge::describe(Descriptor* out) {
  out->command = command_.descriptor();
  out->reply = reply_.descriptor();
  out->frameSizeBytes = frameBytes_;
  out->bufferSizeFrames = data_.size() / frameBytes_;
  out->audio.set<Descriptor::AudioBuffer::Tag::fmq>(data_.descriptor());
}
ndk::ScopedAStatus StreamInBridge::close() {
  std::lock_guard lock(closeMutex_);
  closed_ = true;
  command_.wake(2);
  if (worker_.joinable()) worker_.join();
  return ndk::ScopedAStatus::ok();
}
bool StreamInBridge::open() {
  if (pcm_) return true;
  pcm_ = bridge_pcm_open(
    true, input::kRate, channels_, input::kPeriodFrames, input::kPeriods,
    "Android"
  );
  return pcm_ != nullptr;
}
void StreamInBridge::stop() {
  if (pcm_) {
    bridge_pcm_close(pcm_);
    pcm_ = nullptr;
  }
}
void StreamInBridge::run() {
  std::vector<int8_t> samples(data_.size());
  Descriptor::Command command;
  while (command_.readBlocking(&command, closed_)) {
    Descriptor::Reply reply;
    using Tag = Descriptor::Command::Tag;
    switch (command.getTag()) {
      case Tag::getStatus:
        break;
      case Tag::start:
        if (!open())
          reply.status = -ENODEV;
        else
          state_ = Descriptor::State::IDLE;
        break;
      case Tag::burst: {
        auto bytes = command.get<Tag::burst>();
        if (!input::validBurst(
              bytes, frameBytes_, data_.size() - data_.available()
            )) {
          reply.status = -EINVAL;
          break;
        }
        if (!open()) {
          reply.status = -ENODEV;
          break;
        }
        // Read directly into the FMQ transfer. bridge_pcm blocks on the
        // bridge's capture clock and supplies paced silence when the bridge or
        // source is unavailable, so no second pipe or pacer is needed.
        int status = bridge_pcm_read(pcm_, samples.data(), bytes);
        if (muted_->load())
          std::fill(samples.begin(), samples.begin() + bytes, 0);
        if (status < 0 || !data_.write(samples.data(), bytes)) {
          reply.status = status < 0 ? status : -EIO;
          break;
        }
        read_ += bytes / frameBytes_;
        reply.fmqByteCount = bytes;
        state_ = Descriptor::State::ACTIVE;
        break;
      }
      case Tag::pause:
        state_ = Descriptor::State::PAUSED;
        break;
      case Tag::flush:
      case Tag::standby:
        stop();
        state_ = Descriptor::State::STANDBY;
        break;
      default:
        reply.status = -ENOSYS;
        break;
    }
    timespec now{};
    clock_gettime(CLOCK_MONOTONIC, &now);
    reply.observable.frames = read_;
    reply.observable.timeNs = now.tv_sec * 1000000000LL + now.tv_nsec;
    reply.latencyMs = input::kNominalLatencyMs;
    reply.state = state_;
    if (!reply_.write(reply)) break;
  }
  stop();
}
}  // namespace innkeeper::audio
