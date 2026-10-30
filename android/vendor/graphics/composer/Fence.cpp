// SPDX-License-Identifier: AGPL-3.0-or-later
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-buf.h>
#include <linux/sync_file.h>
#include <poll.h>
#include <sys/ioctl.h>

#include "Fence.h"

namespace linuxapk::hwc {

UniqueFd completionFence(int dmaBuf, int acquireFence) {
  sync_file_info info{};
  if (acquireFence >= 0 && ioctl(acquireFence, SYNC_IOC_FILE_INFO, &info) == 0)
    return UniqueFd(dupFd(acquireFence));
  dma_buf_export_sync_file fence{.flags = DMA_BUF_SYNC_READ, .fd = -1};
  if (dmaBuf >= 0 && ioctl(dmaBuf, DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &fence) == 0)
    return UniqueFd(fence.fd);
  return {};
}

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

}  // namespace linuxapk::hwc
