// SPDX-License-Identifier: AGPL-3.0-only
#pragma once

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <climits>
#include <cstring>
#include <stdexcept>
#include <utility>

#include <aidl/android/hardware/common/fmq/MQDescriptor.h>
#include <aidl/android/hardware/common/fmq/SynchronizedReadWrite.h>
#include <linux/futex.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

namespace linuxapk::audio {
// Synchronized FMQ ABI: byte counters, ring storage and an EventFlag word.
// The descriptors and notification bits are defined by
// android.hardware.common.fmq and libfmq's MessageQueueBase (NOT_FULL=1,
// NOT_EMPTY=2).
template <class T>
class Queue {
 public:
  using Descriptor = aidl::android::hardware::common::fmq::MQDescriptor<
    T, aidl::android::hardware::common::fmq::SynchronizedReadWrite>;
  explicit Queue(size_t count)
      : capacity_(count * sizeof(T)), size_(24 + capacity_) {
    fd_ = memfd_create("linuxapk-audio-fmq", MFD_CLOEXEC);
    if (fd_ < 0) throw std::runtime_error("FMQ memfd");
    if (ftruncate(fd_, size_) != 0) {
      close(fd_);
      throw std::runtime_error("FMQ allocation");
    }
    map_ = static_cast<uint8_t*>(
      mmap(nullptr, size_, PROT_READ | PROT_WRITE, MAP_SHARED, fd_, 0)
    );
    if (map_ == MAP_FAILED) {
      close(fd_);
      throw std::runtime_error("FMQ mmap");
    }
  }
  ~Queue() {
    munmap(map_, size_);
    close(fd_);
  }
  Queue(const Queue&) = delete;
  Queue& operator=(const Queue&) = delete;
  size_t size() const { return capacity_ / sizeof(T); }
  Descriptor descriptor() const {
    Descriptor d;
    d.quantum = sizeof(T);
    d.flags = 1;  // kSynchronizedReadWrite
    int fd = dup(fd_);
    if (fd < 0) throw std::runtime_error("FMQ duplicate");
    d.handle.fds.emplace_back(fd);
    for (auto [offset, extent] :
         {std::pair<size_t, size_t>{0, 8}, {8, 8}, {24, capacity_}, {16, 4}}) {
      aidl::android::hardware::common::fmq::GrantorDescriptor g;
      g.fdIndex = 0;
      g.offset = offset;
      g.extent = extent;
      d.grantors.push_back(g);
    }
    return d;
  }
  size_t available() const {
    auto r = counter(0)->load(std::memory_order_acquire);
    auto w = counter(8)->load(std::memory_order_acquire);
    return w - r <= capacity_ ? (w - r) / sizeof(T) : 0;
  }
  bool read(T* dst, size_t count) {
    auto r = counter(0)->load(std::memory_order_relaxed);
    auto w = counter(8)->load(std::memory_order_acquire);
    size_t bytes = count * sizeof(T);
    if (w - r > capacity_ || bytes > w - r) return false;
    size_t offset = r % capacity_, first = std::min(bytes, capacity_ - offset);
    memcpy(dst, map_ + 24 + offset, first);
    memcpy(reinterpret_cast<uint8_t*>(dst) + first, map_ + 24, bytes - first);
    counter(0)->store(r + bytes, std::memory_order_release);
    wake(1);
    return true;
  }
  bool write(const T& value) {
    auto w = counter(8)->load(std::memory_order_relaxed);
    auto r = counter(0)->load(std::memory_order_acquire);
    if (w - r > capacity_ - sizeof(T)) return false;
    size_t offset = w % capacity_,
           first = std::min(sizeof(T), capacity_ - offset);
    memcpy(map_ + 24 + offset, &value, first);
    memcpy(
      map_ + 24, reinterpret_cast<const uint8_t*>(&value) + first,
      sizeof(T) - first
    );
    counter(8)->store(w + sizeof(T), std::memory_order_release);
    wake(2);
    return true;
  }
  bool readBlocking(T* dst, const std::atomic<bool>& closed) {
    while (!closed.load()) {
      if (read(dst, 1)) return true;
      auto value = event()->fetch_and(~2u);
      if (!(value & 2))
        syscall(
          SYS_futex, event(), FUTEX_WAIT_BITSET, value & ~2u, nullptr, nullptr,
          2
        );
    }
    return false;
  }
  void wake(uint32_t bits) {
    event()->fetch_or(bits);
    syscall(
      SYS_futex, event(), FUTEX_WAKE_BITSET, INT_MAX, nullptr, nullptr, bits
    );
  }

 private:
  std::atomic<uint64_t>* counter(size_t offset) const {
    return reinterpret_cast<std::atomic<uint64_t>*>(map_ + offset);
  }
  std::atomic<uint32_t>* event() const {
    return reinterpret_cast<std::atomic<uint32_t>*>(map_ + 16);
  }
  int fd_;
  uint8_t* map_;
  size_t capacity_, size_;
};
}  // namespace linuxapk::audio
