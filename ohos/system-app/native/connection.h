// SPDX-License-Identifier: AGPL-3.0-or-later
// One bridge connection for the system app's ArkTS, with a reader thread that
// connects, delivers events and replies, and reconnects after the bridge
// restarts. Requests go out from any thread and get their replies by tag.
// napi.c exposes it to ArkTS. It has no N-API dependency, so the host tests
// drive it over a socketpair.
#pragma once

#include <innkeeper/bridge_client.h>
#include <pthread.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The requests that can wait for a reply on one connection.
#define IK_MAX_PENDING 32

// What the reader thread reports. Each callback runs on the reader thread.
struct ik_callbacks {
  // Returns a socket that has passed HELLO, or -errno. Production uses
  // lxb_connect.
  int (*connect)(void* ctx);
  // A new connection is up.
  void (*connected)(void* ctx);
  // An event. The callee owns the packet's fds.
  void (*event)(void* ctx, struct lxb_packet* p);
  // The reply to the request that `cookie` names. The callee owns its fds.
  void (*reply)(void* ctx, void* cookie, struct lxb_packet* p);
  // The request that `cookie` names got no reply, because the connection
  // closed.
  void (*failed)(void* ctx, void* cookie, int err);
  void* ctx;
};

struct ik_pending {
  uint32_t tag;  // 0 when the slot is free
  void* cookie;
};

struct ik_connection {
  struct ik_callbacks cb;
  pthread_mutex_t lock;
  int sock;  // -1 while disconnected
  uint32_t next_tag;
  struct ik_pending pending[IK_MAX_PENDING];
  pthread_t thread;
  bool stop;
};

// Starts the reader thread, which connects at once and then at most once a
// second until it succeeds. Returns 0 or -errno.
int ik_connection_start(struct ik_connection* c, const struct ik_callbacks* cb);

// Sends a request of `type` with `body` and, unless it is -1, `fd`, and
// records `cookie` for its reply. The caller keeps `fd`. Returns 0, or
// -ENOTCONN while disconnected, -EBUSY when IK_MAX_PENDING requests wait, or
// the send's -errno. On failure no callback names `cookie`.
int ik_connection_request(
  struct ik_connection* c, uint16_t type, const void* body, size_t len, int fd,
  void* cookie
);

// Stops the reader thread and fails the waiting requests with -ECANCELED.
// The host tests use it, and the app's connections live as long as its
// process.
void ik_connection_stop(struct ik_connection* c);

#ifdef __cplusplus
}
#endif
