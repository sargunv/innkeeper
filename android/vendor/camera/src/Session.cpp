// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-camera"

#include <algorithm>
#include <cerrno>
#include <ctime>

#include <poll.h>
#include <unistd.h>

#include "Session.h"

#include "Metadata.h"
#include "innkeeper/log.h"

namespace innkeeper::camera {

using aidl::android::hardware::graphics::common::BufferUsage;
using aidl::android::hardware::graphics::common::PixelFormat;
using device::BufferStatus;
using device::CameraMetadata;
using device::CaptureRequest;
using device::CaptureResult;
using device::ErrorCode;
using device::ErrorMsg;
using device::HalStream;
using device::NotifyMsg;
using device::ShutterMsg;
using device::Stream;
using device::StreamBuffer;
using device::StreamConfiguration;
using ndk::ScopedAStatus;

namespace {

// android.hardware.camera.common.Status.
constexpr int32_t kIllegalArgument = 1, kOperationNotSupported = 4,
                  kInternalError = 6;

ScopedAStatus error(int32_t status) {
  return ScopedAStatus::fromServiceSpecificError(status);
}

int64_t monotonicNs() {
  timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return int64_t(ts.tv_sec) * 1'000'000'000 + ts.tv_nsec;
}

bool supported(const Stream& s) {
  if (s.streamType != device::StreamType::OUTPUT) return false;
  if (s.rotation != device::StreamRotation::ROTATION_0) return false;
  int32_t f = int32_t(s.format);
  if (f != kFormatImplDefined && f != kFormatYcbcr420 && f != kFormatBlob)
    return false;
  return std::any_of(std::begin(kSizes), std::end(kSizes), [&](const Size& z) {
    return z.width == s.width && z.height == s.height;
  });
}

// A black NV12 frame.
Frame black(uint32_t w, uint32_t h) {
  Frame f;
  f.width = w;
  f.height = h;
  f.data.assign(size_t(w) * h, 16);
  f.data.resize(f.data.size() + 2 * size_t((w + 1) / 2) * ((h + 1) / 2), 128);
  return f;
}

}  // namespace

bool streamsSupported(const StreamConfiguration& c) {
  if (c.operationMode != device::StreamConfigurationMode::NORMAL_MODE)
    return false;
  int processed = 0, stalling = 0;
  for (const Stream& s : c.streams) {
    if (!supported(s)) return false;
    (int32_t(s.format) == kFormatBlob ? stalling : processed)++;
  }
  return !c.streams.empty() && processed <= 2 && stalling <= 1;
}

Session::Session(std::shared_ptr<device::ICameraDeviceCallback> callback)
    : mCallback(std::move(callback)) {
  mWorker = std::thread([this] { work(); });
}

Session::~Session() { close(); }

bool Session::isClosed() {
  std::lock_guard lock(mMutex);
  return mClosed;
}

ScopedAStatus Session::close() {
  {
    std::lock_guard lock(mMutex);
    if (mClosed) return ScopedAStatus::ok();
    mClosed = true;
  }
  mCond.notify_all();
  if (mWorker.joinable()) mWorker.join();
  mHost.stop();
  std::deque<Pending> dropped;
  {
    std::lock_guard lock(mMutex);
    dropped.swap(mQueue);
    mBuffers.clear();
  }
  for (auto& p : dropped) fail(p, false);
  LOGI("session closed");
  return ScopedAStatus::ok();
}

ScopedAStatus Session::configureStreams(
  const StreamConfiguration& config, std::vector<HalStream>* out
) {
  if (!streamsSupported(config)) return error(kIllegalArgument);
  Size largest{0, 0};
  {
    std::unique_lock lock(mMutex);
    if (mClosed) return error(kInternalError);
    if (!mQueue.empty()) return error(kInternalError);
    // The host stream may restart below, and the worker reads mFrame.
    mCond.wait(lock, [&] { return !mBusy; });
    // cameraserver keeps sending a retained stream's buffers by id alone,
    // so only removed streams lose their imported buffers.
    mStreams.clear();
    for (const Stream& s : config.streams) mStreams[s.id] = s;
    std::erase_if(mBuffers, [&](const auto& e) {
      return !mStreams.count(e.first.first);
    });
    out->clear();
    for (const Stream& s : config.streams) {
      if (s.width * s.height > largest.width * largest.height)
        largest = {s.width, s.height};
      HalStream h;
      h.id = s.id;
      // Our allocator resolves IMPLEMENTATION_DEFINED with CAMERA_OUTPUT to
      // NV12. CPU writes keep the buffer in linear, mappable memory.
      h.overrideFormat = s.format;
      h.producerUsage = BufferUsage(
        int64_t(BufferUsage::CPU_WRITE_OFTEN) |
        int64_t(BufferUsage::CAMERA_OUTPUT)
      );
      h.consumerUsage = BufferUsage(0);
      h.maxBuffers = 4;
      h.overrideDataSpace = s.dataSpace;
      h.supportOffline = false;
      out->push_back(h);
      LOGI(
        "stream %d: %dx%d format %#x usage %#llx", s.id, s.width, s.height,
        int(s.format), (unsigned long long)s.usage
      );
    }
  }
  // The desktop captures at the largest stream's size, so a stream larger
  // than the last request restarts it rather than upscaling its frames.
  HostCamera::State st = mHost.state();
  bool larger =
    largest.width * largest.height > mHostSize.width * mHostSize.height;
  if (
    st == HostCamera::State::Ended ||
    (larger &&
     (st == HostCamera::State::Starting || st == HostCamera::State::Running))
  ) {
    // A new stream numbers its frames from 1 again.
    mFrame.number = 0;
    int err = mHost.start(uint32_t(largest.width), uint32_t(largest.height));
    if (!err) mHostSize = largest;
    if (err == -EACCES) {
      LOGW("the desktop denied the camera earlier in this session");
      return error(kOperationNotSupported);
    }
    if (err)
      LOGW("no host camera stream (%s); frames are black", strerror(-err));
  }
  return ScopedAStatus::ok();
}

ScopedAStatus Session::constructDefaultRequestSettings(
  device::RequestTemplate type, CameraMetadata* out
) {
  out->metadata = defaultRequest(int32_t(type));
  if (out->metadata.empty()) return error(kIllegalArgument);
  return ScopedAStatus::ok();
}

ScopedAStatus Session::flush() {
  std::deque<Pending> dropped;
  {
    std::unique_lock lock(mMutex);
    dropped.swap(mQueue);
    // flush() returns once nothing is outstanding, and the request in
    // progress has an earlier frame number than the dropped ones.
    mCond.wait(lock, [&] { return !mBusy; });
  }
  for (auto& p : dropped) fail(p, false);
  return ScopedAStatus::ok();
}

// Empty descriptors: cameraserver then sends and takes metadata inline.
ScopedAStatus Session::getCaptureRequestMetadataQueue(
  aidl::android::hardware::common::fmq::MQDescriptor<
    int8_t, aidl::android::hardware::common::fmq::SynchronizedReadWrite>*
) {
  return ScopedAStatus::ok();
}

ScopedAStatus Session::getCaptureResultMetadataQueue(
  aidl::android::hardware::common::fmq::MQDescriptor<
    int8_t, aidl::android::hardware::common::fmq::SynchronizedReadWrite>*
) {
  return ScopedAStatus::ok();
}

ScopedAStatus Session::isReconfigurationRequired(
  const CameraMetadata&, const CameraMetadata&, bool* out
) {
  // No session parameters.
  *out = false;
  return ScopedAStatus::ok();
}

ScopedAStatus Session::processCaptureRequest(
  const std::vector<CaptureRequest>& requests,
  const std::vector<device::BufferCache>& cachesToRemove, int32_t* out
) {
  *out = 0;
  std::lock_guard lock(mMutex);
  // Requests queued before a bad one in the same call still go to the worker.
  struct Wake {
    std::condition_variable& c;
    ~Wake() { c.notify_all(); }
  } wake{mCond};
  for (const auto& c : cachesToRemove) mBuffers.erase({c.streamId, c.bufferId});
  if (mClosed) return error(kInternalError);
  for (const CaptureRequest& r : requests) {
    if (
      r.inputBuffer.streamId != -1 || r.outputBuffers.empty() ||
      r.fmqSettingsSize != 0
    ) {
      return error(kIllegalArgument);
    }
    if (!r.settings.metadata.empty() && !validMetadata(r.settings.metadata))
      return error(kIllegalArgument);
    Pending p;
    p.frameNumber = r.frameNumber;
    p.settings = r.settings.metadata;
    for (const StreamBuffer& b : r.outputBuffers) {
      if (!mStreams.count(b.streamId)) {
        closePending(p);
        return error(kIllegalArgument);
      }
      Pending::Out o;
      o.streamId = b.streamId;
      o.bufferId = b.bufferId;
      for (const auto& fd : b.buffer.fds) o.fds.push_back(dup(fd.get()));
      o.ints = b.buffer.ints;
      if (!b.acquireFence.fds.empty())
        o.acquireFence = dup(b.acquireFence.fds[0].get());
      p.outputs.push_back(std::move(o));
    }
    mQueue.push_back(std::move(p));
    (*out)++;
  }
  return ScopedAStatus::ok();
}

ScopedAStatus Session::signalStreamFlush(const std::vector<int32_t>&, int32_t) {
  return ScopedAStatus::ok();
}

ScopedAStatus Session::switchToOffline(
  const std::vector<int32_t>&, device::CameraOfflineSessionInfo*,
  std::shared_ptr<device::ICameraOfflineSession>*
) {
  return error(kOperationNotSupported);
}

ScopedAStatus Session::repeatingRequestEnd(
  int32_t, const std::vector<int32_t>&
) {
  return ScopedAStatus::ok();
}

void Session::closePending(Pending& p) {
  for (auto& o : p.outputs) {
    for (int fd : o.fds) ::close(fd);
    if (o.acquireFence >= 0) ::close(o.acquireFence);
    o.fds.clear();
    o.acquireFence = -1;
  }
}

void Session::work() {
  for (;;) {
    Pending p;
    {
      std::unique_lock lock(mMutex);
      mBusy = false;
      mCond.notify_all();
      mCond.wait(lock, [&] { return mClosed || !mQueue.empty(); });
      if (mClosed) return;
      p = std::move(mQueue.front());
      mQueue.pop_front();
      mBusy = true;
    }
    if (mDeviceError) {
      fail(p, false);
      continue;
    }
    HostCamera::State st = mHost.state();
    if (st == HostCamera::State::Denied || st == HostCamera::State::Lost) {
      LOGW(
        st == HostCamera::State::Denied ? "the desktop denied the camera"
                                        : "the desktop's camera stream ended"
      );
      mDeviceError = true;
      fail(p, true);
      continue;
    }
    process(p);
  }
}

std::shared_ptr<Buffer> Session::buffer(const Pending::Out& o) {
  std::lock_guard lock(mMutex);
  auto key = std::make_pair(o.streamId, o.bufferId);
  if (!o.fds.empty()) mBuffers[key] = Buffer::import(o.fds, o.ints);
  auto it = mBuffers.find(key);
  return it == mBuffers.end() ? nullptr : it->second;
}

bool Session::fill(const Stream& s, Buffer& b, const Frame& f, int quality) {
  Nv12 src{f.data.data(), f.width, f.height};
  if (int32_t(s.format) == kFormatBlob) {
    uint32_t w = uint32_t(s.width), h = uint32_t(s.height);
    uint32_t cw = (w + 1) / 2, ch = (h + 1) / 2;
    std::vector<uint8_t> i420(size_t(w) * h + 2 * size_t(cw) * ch);
    uint8_t* cb = i420.data() + size_t(w) * h;
    YuvTarget t{i420.data(), cb, cb + size_t(cw) * ch, w, h, w, cw, 1};
    scaleNv12(src, t);
    return b.writeJpeg(encodeJpeg(i420.data(), w, h, quality));
  }
  YuvTarget t;
  if (!b.yuv(&t)) {
    LOGE("stream %d: buffer format %#x isn't YCbCr", s.id, b.format());
    return false;
  }
  scaleNv12(src, t);
  return true;
}

void Session::process(Pending& p) {
  if (!p.settings.empty()) mSettings = p.settings;
  if (mFrame.data.empty()) mFrame = black(kSizes[0].width, kSizes[0].height);
  // A result is due one frame interval after the last, so results keep the
  // characteristics' maximum rate. Until then the worker takes the desktop's
  // newest frame, or repeats the last one if the stream hasn't started or has
  // stalled. A faster desktop camera's extra frames are dropped.
  constexpr int64_t kIntervalNs = 1'000'000'000LL / kMaxFps;
  int64_t due = mLastNs + kIntervalNs - monotonicNs();
  Frame next;
  if (
    mHost.next(mFrame.number, int(std::max<int64_t>(due, 0) / 1'000'000), &next)
  ) {
    mFrame = std::move(next);
  }
  due = mLastNs + kIntervalNs - monotonicNs();
  if (due > 0) {
    timespec d{time_t(due / 1'000'000'000), long(due % 1'000'000'000)};
    nanosleep(&d, nullptr);
  }
  int64_t ts = monotonicNs();
  mLastNs = ts;

  std::vector<NotifyMsg> msgs;
  msgs.push_back(
    NotifyMsg::make<NotifyMsg::shutter>(ShutterMsg{
      .frameNumber = p.frameNumber, .timestamp = ts, .readoutTimestamp = ts
    })
  );
  CaptureResult r;
  r.frameNumber = p.frameNumber;
  r.fmqResultSize = 0;
  r.inputBuffer.streamId = -1;
  r.partialResult = 1;
  r.result.metadata = captureResult(mSettings, ts);
  int quality = jpegQuality(mSettings);
  for (auto& o : p.outputs) {
    bool ok = false;
    if (o.acquireFence >= 0) {
      pollfd pfd{o.acquireFence, POLLIN, 0};
      ok = poll(&pfd, 1, 1000) == 1;
    } else {
      ok = true;
    }
    std::shared_ptr<Buffer> b = ok ? buffer(o) : nullptr;
    Stream s;
    {
      std::lock_guard lock(mMutex);
      s = mStreams[o.streamId];
    }
    ok = b && fill(s, *b, mFrame, quality);
    StreamBuffer sb;
    sb.streamId = o.streamId;
    sb.bufferId = o.bufferId;
    sb.status = ok ? BufferStatus::OK : BufferStatus::ERROR;
    if (!ok) {
      msgs.push_back(
        NotifyMsg::make<NotifyMsg::error>(ErrorMsg{
          .frameNumber = p.frameNumber,
          .errorStreamId = o.streamId,
          .errorCode = ErrorCode::ERROR_BUFFER
        })
      );
    }
    r.outputBuffers.push_back(std::move(sb));
  }
  closePending(p);
  mCallback->notify(msgs);
  std::vector<CaptureResult> rs;
  rs.push_back(std::move(r));
  mCallback->processCaptureResult(rs);
}

void Session::fail(Pending& p, bool device) {
  closePending(p);
  ErrorMsg e{
    .frameNumber = device ? -1 : p.frameNumber,
    .errorStreamId = -1,
    .errorCode = device ? ErrorCode::ERROR_DEVICE : ErrorCode::ERROR_REQUEST
  };
  mCallback->notify({NotifyMsg::make<NotifyMsg::error>(e)});
  if (device) return;
  CaptureResult r;
  r.frameNumber = p.frameNumber;
  r.inputBuffer.streamId = -1;
  for (const auto& o : p.outputs) {
    StreamBuffer sb;
    sb.streamId = o.streamId;
    sb.bufferId = o.bufferId;
    sb.status = BufferStatus::ERROR;
    r.outputBuffers.push_back(std::move(sb));
  }
  std::vector<CaptureResult> rs;
  rs.push_back(std::move(r));
  mCallback->processCaptureResult(rs);
}

}  // namespace innkeeper::camera
