// SPDX-License-Identifier: AGPL-3.0-or-later
// Tests the system app's bridge connection over socketpairs, with the test
// acting as the bridge. Requests must carry their tag and fd, replies must
// reach their request, a hangup must fail the waiting requests, and the
// reader must reconnect.
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include "../connection.h"

#define CHECK(cond)                                              \
  do {                                                           \
    if (!(cond)) {                                               \
      fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond); \
      exit(1);                                                   \
    }                                                            \
  } while (0)

// What the callbacks saw, and the bridge's end of the current connection.
static struct {
  pthread_mutex_t lock;
  pthread_cond_t changed;
  int attempts, connects, refuse;
  int bridge;
  int events, event_type, event_fds;
  int replies, reply_status, reply_fds;
  void* reply_cookie;
  uint32_t reply_body;
  int failures, failure_err;
} seen = {PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, .bridge = -1};

static int fake_connect(void* ctx) {
  (void)ctx;
  pthread_mutex_lock(&seen.lock);
  seen.attempts++;
  int r = -ECONNREFUSED;
  if (seen.refuse > 0) {
    seen.refuse--;
  } else {
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) == 0);
    seen.bridge = sv[1];
    r = sv[0];
  }
  pthread_cond_broadcast(&seen.changed);
  pthread_mutex_unlock(&seen.lock);
  return r;
}

static void on_connected(void* ctx) {
  (void)ctx;
  pthread_mutex_lock(&seen.lock);
  seen.connects++;
  pthread_cond_broadcast(&seen.changed);
  pthread_mutex_unlock(&seen.lock);
}

static void on_event(void* ctx, struct lxb_packet* p) {
  (void)ctx;
  pthread_mutex_lock(&seen.lock);
  seen.events++;
  seen.event_type = p->type;
  seen.event_fds = p->nfds;
  lxb_close_fds(p);
  pthread_cond_broadcast(&seen.changed);
  pthread_mutex_unlock(&seen.lock);
}

static void on_reply(void* ctx, void* cookie, struct lxb_packet* p) {
  (void)ctx;
  pthread_mutex_lock(&seen.lock);
  seen.replies++;
  seen.reply_cookie = cookie;
  seen.reply_status = p->status;
  seen.reply_body = p->len == 4 ? lxb_u32(p->body) : 0;
  seen.reply_fds = p->nfds;
  lxb_close_fds(p);
  pthread_cond_broadcast(&seen.changed);
  pthread_mutex_unlock(&seen.lock);
}

static void on_failed(void* ctx, void* cookie, int err) {
  (void)ctx;
  (void)cookie;
  pthread_mutex_lock(&seen.lock);
  seen.failures++;
  seen.failure_err = err;
  pthread_cond_broadcast(&seen.changed);
  pthread_mutex_unlock(&seen.lock);
}

// Waits up to 3 s for `*counter` to reach `want`.
static void wait_for(int* counter, int want) {
  struct timespec deadline;
  clock_gettime(CLOCK_REALTIME, &deadline);
  deadline.tv_sec += 3;
  pthread_mutex_lock(&seen.lock);
  while (*counter < want) {
    CHECK(pthread_cond_timedwait(&seen.changed, &seen.lock, &deadline) == 0);
  }
  pthread_mutex_unlock(&seen.lock);
}

static int bridge(void) {
  pthread_mutex_lock(&seen.lock);
  const int b = seen.bridge;
  pthread_mutex_unlock(&seen.lock);
  return b;
}

static void bridge_send(const uint8_t* b, size_t n, int fd) {
  struct lxb_msg m = {.n = n};
  memcpy(m.b, b, n);
  CHECK(lxb_send(bridge(), &m, fd) == 0);
}

static void bridge_reply(uint32_t tag, int32_t status, uint32_t body, int fd) {
  struct lxb_msg m;
  lxb_msg_init(&m, LXB_REPLY, tag);
  lxb_put_u32(&m, (uint32_t)status);
  lxb_put_u32(&m, body);
  CHECK(lxb_send(bridge(), &m, fd) == 0);
}

// Reads one request on the bridge's end and returns its tag.
static uint32_t bridge_read(uint16_t want_type, size_t want_len, int want_fds) {
  struct lxb_packet p;
  CHECK(lxb_recv(bridge(), &p) == 0);
  CHECK(p.type == want_type && p.len == want_len && p.nfds == want_fds);
  lxb_close_fds(&p);
  return p.tag;
}

int main(void) {
  const struct ik_callbacks cb = {
    .connect = fake_connect,
    .connected = on_connected,
    .event = on_event,
    .reply = on_reply,
    .failed = on_failed,
  };
  static struct ik_connection c;
  int cookie_a, cookie_b;

  // The first attempt fails, so requests fail until the retry a second later
  // connects.
  seen.refuse = 1;
  CHECK(ik_connection_start(&c, &cb) == 0);
  wait_for(&seen.attempts, 1);
  CHECK(ik_connection_request(&c, 0x0505, NULL, 0, -1, &cookie_a) == -ENOTCONN);
  wait_for(&seen.connects, 1);
  CHECK(seen.attempts == 2);

  // A request carries its body and fd. An event before its reply goes to the
  // event callback, and a reply for no waiting request is dropped.
  int pipefd[2];
  CHECK(pipe(pipefd) == 0);
  const uint8_t body[] = {1, 2, 3};
  CHECK(
    ik_connection_request(
      &c, 0x0502, body, sizeof(body), pipefd[0], &cookie_a
    ) == 0
  );
  close(pipefd[0]);
  const uint32_t tag = bridge_read(0x0502, sizeof(body), 1);
  const uint8_t event[] = {0x01, 0x45, 0, 0, 0, 0, 0, 0};
  bridge_send(event, sizeof(event), pipefd[1]);
  bridge_reply(tag + 100, 0, 7, -1);
  bridge_reply(tag, -ENOENT, 42, pipefd[1]);
  close(pipefd[1]);
  wait_for(&seen.replies, 1);
  CHECK(seen.events == 1 && seen.event_type == 0x4501 && seen.event_fds == 1);
  CHECK(seen.replies == 1 && seen.reply_cookie == &cookie_a);
  CHECK(
    seen.reply_status == -ENOENT && seen.reply_body == 42 && seen.reply_fds == 1
  );

  // Bodies over one packet and requests beyond the waiting limit are refused.
  uint8_t big[LXB_MAX_MSG] = {0};
  CHECK(
    ik_connection_request(&c, 0x0502, big, sizeof(big), -1, &cookie_a) ==
    -EMSGSIZE
  );
  for (int i = 0; i < IK_MAX_PENDING; i++) {
    CHECK(ik_connection_request(&c, 0x0505, NULL, 0, -1, &cookie_b) == 0);
  }
  CHECK(ik_connection_request(&c, 0x0505, NULL, 0, -1, &cookie_b) == -EBUSY);

  // A hangup fails every waiting request, and the reader connects again.
  close(bridge());
  wait_for(&seen.failures, IK_MAX_PENDING);
  CHECK(seen.failure_err == -EPIPE);
  wait_for(&seen.connects, 2);
  CHECK(ik_connection_request(&c, 0x0505, NULL, 0, -1, &cookie_b) == 0);
  bridge_read(0x0505, 0, 0);

  // Stopping fails the request still waiting.
  ik_connection_stop(&c);
  CHECK(seen.failures == IK_MAX_PENDING + 1 && seen.failure_err == -ECANCELED);
  puts("ok");
  return 0;
}
