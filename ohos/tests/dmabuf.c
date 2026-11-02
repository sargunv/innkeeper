// SPDX-License-Identifier: AGPL-3.0-or-later
// A test tool for the e2e check of #309. It never ships in the layer. The test
// copies it into the profile and runs it over exec, where OH's shim is
// preloaded.
//
// It makes a dma-buf as the buffer VDI does (ohos/vendor/display/buffer.cpp),
// from a sealed memfd through /dev/udmabuf, and asks for a sync file with
// DMA_BUF_IOCTL_EXPORT_SYNC_FILE. The shim must refuse that with ENOTTY, so
// that the image's llvmpipe offers no native fences. It exits with 0 if the
// export fails with ENOTTY.
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

// linux/udmabuf.h
struct udmabuf_create {
  uint32_t memfd;
  uint32_t flags;
  uint64_t offset;
  uint64_t size;
};
#define UDMABUF_CREATE _IOW('u', 0x42, struct udmabuf_create)

// linux/dma-buf.h
struct dma_buf_export_sync_file {
  uint32_t flags;
  int32_t fd;
};
#define DMA_BUF_SYNC_READ 1
#define DMA_BUF_IOCTL_EXPORT_SYNC_FILE \
  _IOWR('b', 2, struct dma_buf_export_sync_file)

int main(void) {
  int dev = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
  if (dev < 0) {
    fprintf(stderr, "dmabuf: /dev/udmabuf: %s\n", strerror(errno));
    return 2;
  }
  int memfd = memfd_create("dmabuf-test", MFD_CLOEXEC | MFD_ALLOW_SEALING);
  if (
    memfd < 0 || ftruncate(memfd, 4096) != 0 ||
    fcntl(memfd, F_ADD_SEALS, F_SEAL_SHRINK) != 0
  ) {
    fprintf(stderr, "dmabuf: memfd: %s\n", strerror(errno));
    return 2;
  }
  struct udmabuf_create create = {(uint32_t)memfd, 1, 0, 4096};
  int buf = ioctl(dev, UDMABUF_CREATE, &create);
  if (buf < 0) {
    fprintf(stderr, "dmabuf: UDMABUF_CREATE: %s\n", strerror(errno));
    return 2;
  }
  struct dma_buf_export_sync_file sync = {DMA_BUF_SYNC_READ, -1};
  if (ioctl(buf, DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &sync) == 0) {
    printf("dmabuf: EXPORT_SYNC_FILE gave sync file %d\n", sync.fd);
    return 1;
  }
  int e = errno;
  printf("dmabuf: EXPORT_SYNC_FILE: %s\n", strerror(e));
  return e == ENOTTY ? 0 : 1;
}
