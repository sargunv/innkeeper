// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <utility>

#include <unistd.h>

namespace innkeeper::hwc {

// Minimal owning fd, because vendor code cannot use libbase's unique_fd.
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

// Waits for a sync_file, or any pollable fd, to signal. A fence of -1 counts
// as already signalled. Returns false on timeout or error.
bool waitFence(int fd, int timeoutMs);

// Duplicates an fd with F_DUPFD_CLOEXEC. An fd of -1 stays -1.
int dupFd(int fd);

}  // namespace innkeeper::hwc
