// SPDX-License-Identifier: AGPL-3.0-or-later
#define LOG_TAG "innkeeper-camera"

#include <atomic>
#include <cerrno>
#include <chrono>
#include <cstring>

#include <poll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include "HostCamera.h"

#include "innkeeper/bridge_client.h"
#include "innkeeper/log.h"

namespace innkeeper::camera {

namespace {

constexpr uint16_t kInfo = 0x0901, kStart = 0x0902, kStop = 0x0903;
constexpr uint16_t kStatus = 0x4901, kFrame = 0x4902;
constexpr uint32_t kStopTag = 2;
constexpr uint8_t kRunning = 1, kDenied = 2;
constexpr uint32_t kMagic = 0x4643584c;  // "LXCF"
constexpr size_t kDataOffset = 4096, kSlotHeaders = 64;

uint64_t u64(const uint8_t* p) {
  return uint64_t(lxb_u32(p)) | uint64_t(lxb_u32(p + 4)) << 32;
}

size_t nv12Size(uint32_t w, uint32_t h) {
  return size_t(w) * h + 2 * size_t((w + 1) / 2) * ((h + 1) / 2);
}

// lxb_call, keeping the events that arrive before the reply in `events`.
int call(
  int sock, const lxb_msg& req, lxb_packet* reply,
  std::vector<lxb_packet>* events
) {
  reply->nfds = 0;
  if (int r = lxb_send(sock, &req, -1)) return r;
  const uint32_t tag = lxb_u32(req.b + 4);
  auto deadline = std::chrono::steady_clock::now() +
                  std::chrono::milliseconds(LXB_REPLY_TIMEOUT_MS);
  for (;;) {
    auto left = std::chrono::duration_cast<std::chrono::milliseconds>(
      deadline - std::chrono::steady_clock::now()
    );
    pollfd pfd{sock, POLLIN, 0};
    int pr = left.count() > 0 ? poll(&pfd, 1, int(left.count())) : 0;
    if (pr < 0 && errno == EINTR) continue;
    if (pr < 0) return -errno;
    if (pr == 0) return -ETIMEDOUT;
    if (int r = lxb_recv(sock, reply)) return r;
    if (reply->type == LXB_REPLY && reply->tag == tag) return 0;
    lxb_close_fds(reply);
    if (reply->type != LXB_REPLY) events->push_back(*reply);
  }
}

}  // namespace

bool hostCameraPresent() {
  int s = lxb_connect(LXB_SERVICE_CAMERA, "camera-provider");
  if (s < 0) {
    LOGW("no bridge (%s)", strerror(-s));
    return false;
  }
  struct lxb_msg m;
  lxb_msg_init(&m, kInfo, 1);
  struct lxb_packet r;
  bool present = false;
  if (lxb_call(s, &m, &r) == 0) {
    present = r.status == 0 && r.len >= 1 && r.body[0] == 1;
    lxb_close_fds(&r);
  }
  close(s);
  return present;
}

HostCamera::~HostCamera() { stop(); }

int HostCamera::start(uint32_t width, uint32_t height) {
  stop();
  int s = lxb_connect(LXB_SERVICE_CAMERA, "camera-session");
  if (s < 0) return s;
  struct lxb_msg m;
  lxb_msg_init(&m, kStart, 1);
  lxb_put_u16(&m, uint16_t(width));
  lxb_put_u16(&m, uint16_t(height));
  lxb_put_u32(&m, 0);
  struct lxb_packet r;
  // The bridge's stream thread may report, for example a denial, before
  // the bridge replies.
  std::vector<lxb_packet> early;
  int err = call(s, m, &r, &early);
  if (err != 0) {
    close(s);
    return err;
  }
  if (r.status != 0 || r.len < 8 || r.nfds != 1) {
    err = r.status != 0 ? r.status : -EPROTO;
    lxb_close_fds(&r);
    close(s);
    return err;
  }
  uint32_t slots = lxb_u32(r.body), slotBytes = lxb_u32(r.body + 4);
  int fd = r.fds[0];
  size_t size = kDataOffset + size_t(slots) * slotBytes;
  struct stat st;
  void* p = MAP_FAILED;
  if (
    slots > 0 && slots <= 16 && slotBytes > 0 && fstat(fd, &st) == 0 &&
    size_t(st.st_size) >= size
  ) {
    p = mmap(nullptr, size, PROT_READ, MAP_SHARED, fd, 0);
  }
  close(fd);
  if (p == MAP_FAILED || lxb_u32(static_cast<uint8_t*>(p)) != kMagic) {
    if (p != MAP_FAILED) munmap(p, size);
    close(s);
    return -EPROTO;
  }
  mSock = s;
  mRing = static_cast<const uint8_t*>(p);
  mRingSize = size;
  mSlots = slots;
  mSlotBytes = slotBytes;
  {
    std::lock_guard lock(mMutex);
    mState = State::Starting;
    mLatest = 0;
    mStopped = false;
    for (const lxb_packet& e : early) apply(e);
  }
  mReader = std::thread([this] { readEvents(); });
  return 0;
}

void HostCamera::stop() {
  if (mSock < 0) return;
  struct lxb_msg m;
  lxb_msg_init(&m, kStop, kStopTag);
  if (lxb_send(mSock, &m, -1) == 0) {
    // The bridge releases the stream before it answers, so a start() that
    // follows doesn't find the old stream still holding the camera.
    std::unique_lock lock(mMutex);
    mCond.wait_for(lock, std::chrono::milliseconds(LXB_REPLY_TIMEOUT_MS), [&] {
      return mStopped;
    });
  }
  // The reader sees EOF and ends, if the reply didn't end it.
  shutdown(mSock, SHUT_RDWR);
  if (mReader.joinable()) mReader.join();
  close(mSock);
  mSock = -1;
  munmap(const_cast<uint8_t*>(mRing), mRingSize);
  mRing = nullptr;
  std::lock_guard lock(mMutex);
  mState = State::Ended;
  mCond.notify_all();
}

void HostCamera::readEvents() {
  for (;;) {
    struct lxb_packet p;
    if (lxb_recv(mSock, &p) != 0) break;
    lxb_close_fds(&p);
    std::lock_guard lock(mMutex);
    if (p.type == LXB_REPLY && p.tag == kStopTag) {
      mStopped = true;
      mCond.notify_all();
      return;
    }
    if (apply(p)) mCond.notify_all();
  }
  std::lock_guard lock(mMutex);
  if (mState == State::Starting) mState = State::Ended;
  if (mState == State::Running) mState = State::Lost;
  // A closed connection holds no stream either.
  mStopped = true;
  mCond.notify_all();
}

bool HostCamera::apply(const lxb_packet& p) {
  if (p.type == kStatus && p.len >= 1) {
    uint8_t st = p.body[0];
    mState = st == kRunning             ? State::Running
             : st == kDenied            ? State::Denied
             : mState == State::Running ? State::Lost
                                        : State::Ended;
    LOGI("host camera status %u", st);
    return true;
  }
  if (p.type == kFrame && p.len >= 16) {
    uint32_t slot = lxb_u32(p.body);
    uint64_t n = u64(p.body + 8);
    if (slot < mSlots && n > mLatest) {
      mLatestSlot = slot;
      mLatest = n;
    }
    return true;
  }
  return false;
}

HostCamera::State HostCamera::state() {
  std::lock_guard lock(mMutex);
  return mState;
}

bool HostCamera::copySlot(uint32_t slot, Frame* out) {
  const uint8_t* h = mRing + kSlotHeaders + slot * 64;
  auto* seq = reinterpret_cast<const std::atomic<uint64_t>*>(h);
  uint64_t s1 = seq->load(std::memory_order_acquire);
  if (s1 & 1) return false;
  uint32_t w = lxb_u32(h + 8), hh = lxb_u32(h + 12);
  size_t n = nv12Size(w, hh);
  if (w == 0 || hh == 0 || n > mSlotBytes) return false;
  out->width = w;
  out->height = hh;
  out->number = u64(h + 16);
  out->timestampNs = int64_t(u64(h + 24));
  out->data.resize(n);
  memcpy(out->data.data(), mRing + kDataOffset + size_t(slot) * mSlotBytes, n);
  std::atomic_thread_fence(std::memory_order_acquire);
  return seq->load(std::memory_order_relaxed) == s1;
}

bool HostCamera::next(uint64_t after, int timeoutMs, Frame* out) {
  std::unique_lock lock(mMutex);
  bool ready = mCond.wait_for(lock, std::chrono::milliseconds(timeoutMs), [&] {
    return mLatest > after ||
           (mState != State::Starting && mState != State::Running);
  });
  if (!ready || mLatest <= after || !mRing) return false;
  uint32_t slot = mLatestSlot;
  lock.unlock();
  // A slot overwritten while copying is dropped; the next frame comes soon.
  return copySlot(slot, out);
}

}  // namespace innkeeper::camera
