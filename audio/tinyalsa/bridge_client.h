// Client side of the Linux.apk bridge protocol v1 (docs/bridge.md) for code
// running inside Android: one shared connection per process, and output
// streams that write PCM into a shared ring the bridge plays to PipeWire.
//
// Kept free of tinyalsa types so an AIDL StreamBridge can reuse it as is.
#pragma once

#include <stdatomic.h>
#include <stdint.h>
#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

// Where the session binds the bridge socket.
#define LXB_SOCKET_PATH "/dev/socket/linuxapk/bridge.sock"

enum lxb_format { LXB_S16LE = 1, LXB_F32LE = 2 };

struct lxb_out_cfg {
  uint32_t rate;
  uint8_t channels;
  uint8_t format;  // enum lxb_format
  uint32_t period_frames;
  uint32_t periods;
  const char* name;  // shown in the desktop mixer
};

struct lxb_stream {
  uint32_t id;
  uint32_t capacity;  // frames
  uint32_t frame_bytes;
  uint32_t rate;
  uint32_t latency_us;  // bridge's nominal downstream latency
  int data_fd;          // we signal: frames written
  int space_fd;         // bridge signals: frames consumed (or stream died)
  uint8_t* map;
  size_t map_size;
  uint64_t written;  // frames written into this ring (atomic access)
};

// Opens an output stream (connecting to the bridge if needed).
// Returns 0 or -errno.
int lxb_open_output(const struct lxb_out_cfg* cfg, struct lxb_stream* s);
void lxb_close_output(struct lxb_stream* s);
int lxb_standby(struct lxb_stream* s);
// Applies to every stream of this process, including later ones.
int lxb_set_master(float volume, int muted);

// Writes up to `frames` frames. Blocks while the ring is full, up to
// `timeout_ms` in total. Returns frames written (possibly fewer on timeout),
// or -EPIPE if the bridge dropped the stream.
int lxb_write(
  struct lxb_stream* s, const void* buf, uint32_t frames, int timeout_ms
);
// Frames the bridge has taken from the ring.
uint64_t lxb_consumed(const struct lxb_stream* s);
// Frames presented to the listener as of now (CLOCK_MONOTONIC in *ts),
// never more than consumed. Returns 0, or -EAGAIN if there's no estimate yet.
int lxb_presented(
  const struct lxb_stream* s, uint64_t* frames, struct timespec* ts
);
// Whether the bridge marked the stream dead (the client should reopen).
int lxb_dead(const struct lxb_stream* s);

#ifdef __cplusplus
}
#endif
