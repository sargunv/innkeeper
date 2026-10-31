// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <utility>

#include <unistd.h>

namespace innkeeper::hwc {

// Minimal owning fd (libbase's unique_fd is not available to us).
class UniqueFd {
 public:
  UniqueFd() = default;
  explicit UniqueFd(int fd) : mFd(fd) {}
  UniqueFd(UniqueFd&& o) noexcept : mFd(o.release()) {}
  UniqueFd& operator=(UniqueFd&& o) noexcept {
    reset(o.release());
    return *this;
  }
  ~UniqueFd() { reset(); }
  int get() const { return mFd; }
  bool ok() const { return mFd >= 0; }
  int release() { return std::exchange(mFd, -1); }
  void reset(int fd = -1) {
    if (mFd >= 0) close(mFd);
    mFd = fd;
  }

 private:
  int mFd = -1;
};

// Waits for a sync_file (or any pollable fd) to signal. -1 is "already
// signalled". Returns false on timeout/error.
bool waitFence(int fd, int timeoutMs);

// Duplicates an fd (F_DUPFD_CLOEXEC); -1 stays -1.
int dupFd(int fd);

}  // namespace innkeeper::hwc
