// SPDX-License-Identifier: AGPL-3.0-or-later
// See innkeeper/bridge_client.h.

#include <errno.h>
#include <poll.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include "innkeeper/bridge_client.h"

#define LXB_HELLO 0x0001
#define LXB_MAJOR 1
#define LXB_MINOR 0

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

// ---- messages ----

static void put(struct lxb_msg* m, const void* p, size_t n) {
  if (m->n + n > sizeof(m->b)) {
    m->overflow = true;
    return;
  }
  memcpy(m->b + m->n, p, n);
  m->n += n;
}

void lxb_put_u8(struct lxb_msg* m, uint8_t v) { put(m, &v, 1); }

void lxb_put_u16(struct lxb_msg* m, uint16_t v) {
  const uint8_t b[2] = {(uint8_t)v, (uint8_t)(v >> 8)};
  put(m, b, sizeof(b));
}

void lxb_put_u32(struct lxb_msg* m, uint32_t v) {
  lxb_put_u16(m, (uint16_t)v);
  lxb_put_u16(m, (uint16_t)(v >> 16));
}

void lxb_put_u64(struct lxb_msg* m, uint64_t v) {
  lxb_put_u32(m, (uint32_t)v);
  lxb_put_u32(m, (uint32_t)(v >> 32));
}

void lxb_put_f32(struct lxb_msg* m, float v) {
  uint32_t bits;
  memcpy(&bits, &v, sizeof(bits));
  lxb_put_u32(m, bits);
}

void lxb_put_name(struct lxb_msg* m, const char* s) {
  const size_t len = s ? strlen(s) : 0;
  size_t n = len > LXB_MAX_NAME ? LXB_MAX_NAME : len;
  while (n > 0 && n < len && ((uint8_t)s[n] & 0xc0) == 0x80) n--;
  lxb_put_u16(m, (uint16_t)n);
  put(m, s, n);
}

void lxb_msg_init(struct lxb_msg* m, uint16_t type, uint32_t tag) {
  m->n = 0;
  m->overflow = false;
  lxb_put_u16(m, type);
  lxb_put_u16(m, 0);
  lxb_put_u32(m, tag);
}

// ---- packets ----

void lxb_close_fds(struct lxb_packet* p) {
  for (int i = 0; i < p->nfds; i++) close(p->fds[i]);
  p->nfds = 0;
}

int lxb_send(int sock, const struct lxb_msg* m, int fd) {
  if (m->overflow) return -EMSGSIZE;
  struct iovec iov = {.iov_base = (void*)m->b, .iov_len = m->n};
  struct msghdr h = {.msg_iov = &iov, .msg_iovlen = 1};
  union {
    struct cmsghdr align;
    char bytes[CMSG_SPACE(sizeof(int))];
  } ctl;
  if (fd >= 0) {
    h.msg_control = ctl.bytes;
    h.msg_controllen = sizeof(ctl.bytes);
    struct cmsghdr* c = CMSG_FIRSTHDR(&h);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof(fd));
  }
  ssize_t n;
  do {
    n = sendmsg(sock, &h, MSG_NOSIGNAL);
  } while (n < 0 && errno == EINTR);
  if (n < 0) return -errno;
  return (size_t)n == m->n ? 0 : -EMSGSIZE;
}

int lxb_recv(int sock, struct lxb_packet* p) {
  p->nfds = 0;
  uint8_t buf[LXB_MAX_MSG];
  union {
    struct cmsghdr align;
    char bytes[CMSG_SPACE(sizeof(int) * LXB_MAX_FDS)];
  } ctl;
  struct iovec iov = {.iov_base = buf, .iov_len = sizeof(buf)};
  struct msghdr h = {
    .msg_iov = &iov,
    .msg_iovlen = 1,
    .msg_control = ctl.bytes,
    .msg_controllen = sizeof(ctl.bytes),
  };
  ssize_t n;
  do {
    n = recvmsg(sock, &h, MSG_CMSG_CLOEXEC);
  } while (n < 0 && errno == EINTR);
  if (n < 0) return -errno;
  if (n == 0) return -EPIPE;

  for (struct cmsghdr* c = CMSG_FIRSTHDR(&h); c; c = CMSG_NXTHDR(&h, c)) {
    if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS) continue;
    const size_t k = (c->cmsg_len - CMSG_LEN(0)) / sizeof(int);
    for (size_t i = 0; i < k && p->nfds < LXB_MAX_FDS; i++)
      memcpy(&p->fds[p->nfds++], CMSG_DATA(c) + i * sizeof(int), sizeof(int));
  }
  const bool reply = n >= LXB_HEADER && lxb_u16(buf) == LXB_REPLY;
  const size_t head = reply ? LXB_HEADER + 4 : LXB_HEADER;
  if ((size_t)n < head || (h.msg_flags & (MSG_TRUNC | MSG_CTRUNC))) {
    lxb_close_fds(p);
    return -EPROTO;
  }
  p->type = lxb_u16(buf);
  p->tag = lxb_u32(buf + 4);
  p->status = reply ? (int32_t)lxb_u32(buf + LXB_HEADER) : 0;
  p->len = (size_t)n - head;
  memcpy(p->body, buf + head, p->len);
  return 0;
}

int lxb_call(int sock, const struct lxb_msg* req, struct lxb_packet* reply) {
  reply->nfds = 0;
  int r = lxb_send(sock, req, -1);
  if (r != 0) return r;
  const uint32_t tag = lxb_u32(req->b + 4);
  const int64_t deadline = now_ns() + LXB_REPLY_TIMEOUT_MS * 1000000LL;
  for (;;) {
    const int64_t left_ms = (deadline - now_ns()) / 1000000;
    struct pollfd pfd = {.fd = sock, .events = POLLIN};
    const int pr = left_ms > 0 ? poll(&pfd, 1, (int)left_ms) : 0;
    if (pr < 0 && errno == EINTR) continue;
    if (pr < 0) return -errno;
    if (pr == 0) return -ETIMEDOUT;
    r = lxb_recv(sock, reply);
    if (r != 0) return r;
    if (reply->type == LXB_REPLY && reply->tag == tag) return 0;
    lxb_close_fds(reply);  // an event or another request's reply
  }
}

// ---- connecting ----

int lxb_connect(uint32_t service, const char* name) {
  const int s = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
  if (s < 0) return -errno;
  struct sockaddr_un addr = {.sun_family = AF_UNIX};
  strncpy(addr.sun_path, LXB_SOCKET_PATH, sizeof(addr.sun_path) - 1);
  if (connect(s, (struct sockaddr*)&addr, sizeof(addr)) != 0) {
    const int e = errno;
    close(s);
    return -e;
  }
  struct lxb_msg hello;
  lxb_msg_init(&hello, LXB_HELLO, 1);
  lxb_put_u16(&hello, LXB_MAJOR);
  lxb_put_u16(&hello, LXB_MINOR);
  lxb_put_u32(&hello, service);
  lxb_put_name(&hello, name);
  struct lxb_packet reply;
  int r = lxb_call(s, &hello, &reply);
  if (r == 0) {
    lxb_close_fds(&reply);
    r = reply.status;
  }
  if (r != 0) {
    close(s);
    // The bridge refuses a HELLO by hanging up.
    return r == -EPIPE || r > 0 ? -ECONNREFUSED : r;
  }
  return s;
}

int lxb_backoff_ms(struct lxb_backoff* b) {
  const int64_t now = now_ns();
  if (now < b->next_attempt_ns)
    return (int)((b->next_attempt_ns - now + 999999) / 1000000);
  b->next_attempt_ns = now + 1000000000LL;
  return 0;
}
