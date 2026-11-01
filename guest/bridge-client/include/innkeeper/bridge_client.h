// SPDX-License-Identifier: AGPL-3.0-or-later
// The client side of bridge protocol v1 (docs/bridge-protocol.md) for guest
// code, mirroring host/bridge/src/ipc/{wire,transport,proto}.rs in C. It
// connects and sends HELLO, builds messages, sends and receives packets with
// fds, and matches replies to requests by tag. Each caller, such as the
// composer's display presenter or the audio HAL's streams, owns its socket
// and its service's messages.
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Where the session binds the bridge socket.
#define LXB_SOCKET_PATH "/dev/socket/innkeeper/bridge.sock"

#define LXB_HEADER 8     // u16 type, u16 flags (0), u32 tag
#define LXB_MAX_MSG 512  // header and body
#define LXB_MAX_FDS 4    // per packet
#define LXB_MAX_NAME 96  // UTF-8 bytes
#define LXB_REPLY 0x8000
// How long lxb_connect and lxb_call wait for a reply.
#define LXB_REPLY_TIMEOUT_MS 3000

// The services with a client here (host/bridge/src/ipc/proto.rs).
enum {
  LXB_SERVICE_AUDIO = 1,
  LXB_SERVICE_DISPLAY = 2,
  LXB_SERVICE_HOSTSTATE = 8,
};

// An outgoing message. lxb_msg_init writes the header, and the lxb_put_*
// functions append little-endian fields to the body.
struct lxb_msg {
  uint8_t b[LXB_MAX_MSG];
  size_t n;
  bool overflow;  // set when a field didn't fit, so lxb_send refuses it
};

void lxb_msg_init(struct lxb_msg* m, uint16_t type, uint32_t tag);
void lxb_put_u8(struct lxb_msg* m, uint8_t v);
void lxb_put_u16(struct lxb_msg* m, uint16_t v);
void lxb_put_u32(struct lxb_msg* m, uint32_t v);
void lxb_put_u64(struct lxb_msg* m, uint64_t v);
void lxb_put_f32(struct lxb_msg* m, float v);
// Appends a u16 length, then at most LXB_MAX_NAME bytes of `s`, cut on a
// UTF-8 boundary. A NULL `s` is the empty name.
void lxb_put_name(struct lxb_msg* m, const char* s);

// An incoming packet. For a reply, `status` holds the reply's status and
// `body` holds the bytes after it.
struct lxb_packet {
  uint16_t type;
  uint32_t tag;
  int32_t status;  // replies only: 0 or -errno
  uint8_t body[LXB_MAX_MSG];
  size_t len;
  int fds[LXB_MAX_FDS];  // owned by the receiver, which keeps or closes them
  int nfds;
};

static inline uint16_t lxb_u16(const uint8_t* p) {
  return (uint16_t)(p[0] | p[1] << 8);
}
static inline uint32_t lxb_u32(const uint8_t* p) {
  return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 |
         (uint32_t)p[3] << 24;
}

void lxb_close_fds(struct lxb_packet* p);

// Connects to the bridge and binds the connection to `service` with HELLO.
// The bridge logs `name`. Returns the socket or -errno. The error is -ENOENT
// or -ECONNREFUSED if no bridge listens, -ECONNREFUSED if the bridge refused
// the HELLO, and -ETIMEDOUT if it didn't answer.
int lxb_connect(uint32_t service, const char* name);

// Sends one message, with `fd` attached unless it is -1. A message is one
// packet, so several threads may send on one socket. Returns 0 or -errno.
int lxb_send(int sock, const struct lxb_msg* m, int fd);

// Waits for the next packet. Returns 0, -EPIPE if the bridge hung up,
// -EPROTO for a malformed packet, or another -errno. On failure *p holds no
// fds.
int lxb_recv(int sock, struct lxb_packet* p);

// Sends a request and waits for its reply, dropping events and other
// replies meanwhile, so no other thread may receive on `sock`. Returns 0
// with the reply in *reply, or -errno if the connection failed. After a
// failure the caller should close `sock`, and *reply holds no fds.
int lxb_call(int sock, const struct lxb_msg* req, struct lxb_packet* reply);

// Paces reconnecting to a bridge that is missing or restarting to one
// attempt per second. Zero-initialize it.
struct lxb_backoff {
  int64_t next_attempt_ns;
};
// Returns 0 if an attempt is due now and schedules the next one a second
// later. Otherwise returns the milliseconds until the next attempt is due.
int lxb_backoff_ms(struct lxb_backoff* b);

#ifdef __cplusplus
}
#endif
