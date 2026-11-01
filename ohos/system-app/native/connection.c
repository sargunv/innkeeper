// SPDX-License-Identifier: AGPL-3.0-or-later
// See connection.h.

#include <errno.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include "connection.h"

// Takes every waiting request off `c`. The caller holds the lock.
static size_t take_pending(struct ik_connection* c, void** cookies) {
  size_t n = 0;
  for (size_t i = 0; i < IK_MAX_PENDING; i++) {
    if (c->pending[i].tag == 0) continue;
    cookies[n++] = c->pending[i].cookie;
    c->pending[i].tag = 0;
  }
  return n;
}

static void* take_reply(struct ik_connection* c, uint32_t tag, bool* found) {
  *found = false;
  void* cookie = NULL;
  pthread_mutex_lock(&c->lock);
  for (size_t i = 0; i < IK_MAX_PENDING; i++) {
    if (c->pending[i].tag != tag) continue;
    cookie = c->pending[i].cookie;
    c->pending[i].tag = 0;
    *found = true;
    break;
  }
  pthread_mutex_unlock(&c->lock);
  return cookie;
}

static bool stopping(struct ik_connection* c) {
  pthread_mutex_lock(&c->lock);
  const bool stop = c->stop;
  pthread_mutex_unlock(&c->lock);
  return stop;
}

// Reads packets until the connection fails, then fails its waiting requests.
static void serve(struct ik_connection* c, int sock) {
  for (;;) {
    struct lxb_packet p;
    if (lxb_recv(sock, &p) != 0) break;
    if (p.type != LXB_REPLY) {
      c->cb.event(c->cb.ctx, &p);
      continue;
    }
    bool found;
    void* cookie = take_reply(c, p.tag, &found);
    if (found) {
      c->cb.reply(c->cb.ctx, cookie, &p);
    } else {
      lxb_close_fds(&p);
    }
  }
  void* cookies[IK_MAX_PENDING];
  pthread_mutex_lock(&c->lock);
  c->sock = -1;
  const size_t n = take_pending(c, cookies);
  const int err = c->stop ? -ECANCELED : -EPIPE;
  pthread_mutex_unlock(&c->lock);
  close(sock);
  for (size_t i = 0; i < n; i++) c->cb.failed(c->cb.ctx, cookies[i], err);
}

static void* reader(void* arg) {
  struct ik_connection* c = arg;
  struct lxb_backoff backoff = {0};
  while (!stopping(c)) {
    const int wait_ms = lxb_backoff_ms(&backoff);
    if (wait_ms > 0) {
      const struct timespec ts = {
        wait_ms / 1000, (long)(wait_ms % 1000) * 1000000
      };
      nanosleep(&ts, NULL);
      continue;
    }
    const int sock = c->cb.connect(c->cb.ctx);
    if (sock < 0) continue;
    pthread_mutex_lock(&c->lock);
    const bool stop = c->stop;
    if (!stop) c->sock = sock;
    pthread_mutex_unlock(&c->lock);
    if (stop) {
      close(sock);
      break;
    }
    c->cb.connected(c->cb.ctx);
    serve(c, sock);
  }
  return NULL;
}

int ik_connection_start(
  struct ik_connection* c, const struct ik_callbacks* cb
) {
  memset(c, 0, sizeof(*c));
  c->cb = *cb;
  c->sock = -1;
  c->next_tag = 1;
  pthread_mutex_init(&c->lock, NULL);
  return -pthread_create(&c->thread, NULL, reader, c);
}

int ik_connection_request(
  struct ik_connection* c, uint16_t type, const void* body, size_t len, int fd,
  void* cookie
) {
  if (len > LXB_MAX_MSG - LXB_HEADER) return -EMSGSIZE;
  pthread_mutex_lock(&c->lock);
  int r = -ENOTCONN;
  if (c->sock < 0) goto out;
  struct ik_pending* slot = NULL;
  for (size_t i = 0; i < IK_MAX_PENDING && !slot; i++) {
    if (c->pending[i].tag == 0) slot = &c->pending[i];
  }
  r = -EBUSY;
  if (!slot) goto out;
  const uint32_t tag = c->next_tag++;
  if (c->next_tag == 0) c->next_tag = 1;
  struct lxb_msg m;
  lxb_msg_init(&m, type, tag);
  memcpy(m.b + m.n, body, len);
  m.n += len;
  // The lock covers the send, so the reader can't close the socket under it,
  // and the slot is filled first, so the reply finds it.
  *slot = (struct ik_pending){.tag = tag, .cookie = cookie};
  r = lxb_send(c->sock, &m, fd);
  if (r != 0) slot->tag = 0;
out:
  pthread_mutex_unlock(&c->lock);
  return r;
}

void ik_connection_stop(struct ik_connection* c) {
  pthread_mutex_lock(&c->lock);
  c->stop = true;
  if (c->sock >= 0) shutdown(c->sock, SHUT_RDWR);
  pthread_mutex_unlock(&c->lock);
  pthread_join(c->thread, NULL);
  pthread_mutex_destroy(&c->lock);
}
