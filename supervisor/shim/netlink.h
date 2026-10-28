// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once
#include <linux/netlink.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>

// Separate Flatpak pid namespaces share the host network. Uevent receivers
// must let the kernel choose a port instead of colliding on their local pid.
static int linuxapk_netlink_bind(
  int fd, const struct sockaddr* addr, socklen_t len, int flatpak
) {
  if (
    flatpak && addr && len >= sizeof(struct sockaddr_nl) &&
    addr->sa_family == AF_NETLINK
  ) {
    int protocol;
    socklen_t size = sizeof(protocol);
    if (
      getsockopt(fd, SOL_SOCKET, SO_PROTOCOL, &protocol, &size) == 0 &&
      protocol == NETLINK_KOBJECT_UEVENT
    ) {
      struct sockaddr_nl local = *(const struct sockaddr_nl*)addr;
      local.nl_pid = 0;
      return syscall(SYS_bind, fd, &local, sizeof(local));
    }
  }
  return syscall(SYS_bind, fd, addr, len);
}
