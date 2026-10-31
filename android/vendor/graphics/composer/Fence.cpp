// SPDX-License-Identifier: AGPL-3.0-or-later
#include <errno.h>
#include <fcntl.h>
#include <poll.h>

#include "Fence.h"

namespace innkeeper::hwc {

bool waitFence(int fd, int timeoutMs) {
  if (fd < 0) return true;
  struct pollfd p = {fd, POLLIN, 0};
  int ret;
  do {
    ret = poll(&p, 1, timeoutMs);
  } while (ret < 0 && (errno == EINTR || errno == EAGAIN));
  return ret > 0 && !(p.revents & (POLLERR | POLLNVAL));
}

int dupFd(int fd) { return fd < 0 ? -1 : fcntl(fd, F_DUPFD_CLOEXEC, 0); }

}  // namespace innkeeper::hwc
