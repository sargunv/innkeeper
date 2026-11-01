// SPDX-License-Identifier: AGPL-3.0-or-later
// Tests the bridge client's framing over a socketpair, with the test acting
// as the bridge. Messages must encode as docs/bridge-protocol.md specifies,
// fds must cross, and lxb_call must skip events to find its reply.
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#include "innkeeper/bridge_client.h"

#define CHECK(cond)                                              \
  do {                                                           \
    if (!(cond)) {                                               \
      fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond); \
      exit(1);                                                   \
    }                                                            \
  } while (0)

// Sends a raw packet from the bridge's side, optionally with an fd.
static void bridge_send(int sock, const uint8_t* b, size_t n, int fd) {
  struct lxb_msg m = {.n = n};
  memcpy(m.b, b, n);
  CHECK(lxb_send(sock, &m, fd) == 0);
}

static void test_encoding(int client, int bridge) {
  struct lxb_msg m;
  lxb_msg_init(&m, 0x0201, 7);
  lxb_put_u8(&m, 0xab);
  lxb_put_u16(&m, 0x1234);
  lxb_put_u32(&m, 0xdeadbeef);
  lxb_put_u64(&m, 0x0102030405060708ull);
  lxb_put_f32(&m, 1.0f);
  lxb_put_name(&m, "hi");
  CHECK(lxb_send(client, &m, -1) == 0);

  uint8_t got[LXB_MAX_MSG];
  const uint8_t want[] = {
    0x01, 0x02, 0,    0,    7,    0,    0,    0,  // header
    0xab, 0x34, 0x12, 0xef, 0xbe, 0xad, 0xde,     // u8, u16, u32
    8,    7,    6,    5,    4,    3,    2,    1,  // u64
    0,    0,    0x80, 0x3f,                       // 1.0f
    2,    0,    'h',  'i',                        // name
  };
  CHECK(recv(bridge, got, sizeof(got), 0) == sizeof(want));
  CHECK(memcmp(got, want, sizeof(want)) == 0);
}

static void test_name_cut_on_utf8_boundary(void) {
  char name[LXB_MAX_NAME + 2];
  memset(name, 'a', LXB_MAX_NAME - 1);
  strcpy(name + LXB_MAX_NAME - 1, "\xc3\xa9");  // é straddles the limit
  struct lxb_msg m;
  lxb_msg_init(&m, 1, 1);
  lxb_put_name(&m, name);
  CHECK(lxb_u16(m.b + LXB_HEADER) == LXB_MAX_NAME - 1);
}

static void test_overflow_refused(int client) {
  struct lxb_msg m;
  lxb_msg_init(&m, 1, 1);
  for (int i = 0; i < LXB_MAX_MSG; i++) lxb_put_u8(&m, 0);
  CHECK(m.overflow);
  CHECK(lxb_send(client, &m, -1) == -EMSGSIZE);
}

// lxb_call skips an event and closes its fd, then returns the reply with its
// status split off and its fd attached.
static void test_call(int client, int bridge) {
  int pipefd[2];
  CHECK(pipe(pipefd) == 0);
  const uint8_t event[] = {0x01, 0x42, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0};
  bridge_send(bridge, event, sizeof(event), pipefd[0]);
  const uint8_t reply[] = {
    0x00, 0x80, 0,    0,    5, 0, 0, 0,  // REPLY, tag 5
    0xfe, 0xff, 0xff, 0xff,              // status -ENOENT
    42,   0,    0,    0,                 // body
  };
  bridge_send(bridge, reply, sizeof(reply), pipefd[1]);
  close(pipefd[0]);
  close(pipefd[1]);

  struct lxb_msg req;
  lxb_msg_init(&req, 0x0102, 5);
  struct lxb_packet p;
  CHECK(lxb_call(client, &req, &p) == 0);
  CHECK(p.type == LXB_REPLY && p.tag == 5 && p.status == -ENOENT);
  CHECK(p.len == 4 && lxb_u32(p.body) == 42);
  CHECK(p.nfds == 1);
  lxb_close_fds(&p);

  uint8_t sent[LXB_MAX_MSG];
  CHECK(recv(bridge, sent, sizeof(sent), 0) == LXB_HEADER);
  CHECK(lxb_u16(sent) == 0x0102 && lxb_u32(sent + 4) == 5);
}

static void test_malformed_and_hangup(int client, int bridge) {
  const uint8_t short_reply[] = {0x00, 0x80, 0, 0, 1, 0, 0, 0, 0};
  bridge_send(bridge, short_reply, sizeof(short_reply), -1);
  struct lxb_packet p;
  CHECK(lxb_recv(client, &p) == -EPROTO);
  close(bridge);
  CHECK(lxb_recv(client, &p) == -EPIPE);
}

static void test_backoff(void) {
  struct lxb_backoff b = {0};
  CHECK(lxb_backoff_ms(&b) == 0);
  const int wait = lxb_backoff_ms(&b);
  CHECK(wait > 900 && wait <= 1000);
}

int main(void) {
  int sv[2];
  CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) == 0);
  test_encoding(sv[0], sv[1]);
  test_name_cut_on_utf8_boundary();
  test_overflow_refused(sv[0]);
  test_call(sv[0], sv[1]);
  test_malformed_and_hangup(sv[0], sv[1]);
  test_backoff();
  close(sv[0]);
  puts("ok");
  return 0;
}
