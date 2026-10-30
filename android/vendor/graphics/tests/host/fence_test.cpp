// SPDX-License-Identifier: AGPL-3.0-or-later
#include <cstdlib>

#include <fcntl.h>
#include <sys/eventfd.h>

#include "Fence.h"
using namespace linuxapk::hwc;
#define CHECK(x)            \
  do {                      \
    if (!(x)) std::abort(); \
  } while (0)
int main() {
  CHECK(waitFence(-1, 0));
  CHECK(!completionFence(-1).ok());
  const int fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
  CHECK(fd >= 0);
  // An eventfd is pollable but not a sync_file and must never escape as one.
  CHECK(!completionFence(-1, fd).ok());
  CHECK(!completionFence(fd).ok());
  CHECK(!waitFence(fd, 0));
  int duplicate;
  {
    UniqueFd owned(fd);
    UniqueFd moved(std::move(owned));
    CHECK(!owned.ok() && moved.get() == fd);
    duplicate = dupFd(fd);
    CHECK(duplicate >= 0 && (fcntl(duplicate, F_GETFD) & FD_CLOEXEC));
    UniqueFd other(duplicate);
    CHECK(eventfd_write(fd, 1) == 0 && waitFence(duplicate, 0));
    other = std::move(moved);  // Closes its previous fence exactly once.
    CHECK(fcntl(duplicate, F_GETFD) == -1);
    CHECK(!moved.ok() && other.get() == fd);
  }
  CHECK(fcntl(fd, F_GETFD) == -1);
  CHECK(!waitFence(fd, 0));
  CHECK(dupFd(-1) == -1);
}
