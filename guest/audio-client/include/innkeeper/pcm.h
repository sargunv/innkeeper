// SPDX-License-Identifier: AGPL-3.0-or-later
// One bridge audio stream (audio_client.h) as a blocking 16-bit PCM device,
// for the guests' audio HALs.
//
// - The bridge stream opens lazily on the first read/write.
// - Writes block while the ring is full, pacing the HAL by the host's audio
//   clock. Without a bridge, writes are paced by CLOCK_MONOTONIC and dropped,
//   and reads return paced silence. Reconnects are retried once a second.
// - The delay comes from the bridge's presentation estimate, which includes
//   PipeWire's queue and the device.
#pragma once

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct bridge_pcm;

// Opens an S16LE device at `rate` with 1 or 2 channels. The desktop mixer
// shows the stream as `name` until a usage is reported. `name` must outlive
// the device. Returns NULL if the config is unsupported.
struct bridge_pcm* bridge_pcm_open(
  bool input, uint32_t rate, uint32_t channels, uint32_t period_frames,
  uint32_t periods, const char* name
);
void bridge_pcm_close(struct bridge_pcm* p);
// Transfers the whole buffer. Returns 0, or -errno.
int bridge_pcm_write(struct bridge_pcm* p, const void* data, uint32_t bytes);
int bridge_pcm_read(struct bridge_pcm* p, void* data, uint32_t bytes);
// Frames written but not yet presented.
long bridge_pcm_delay(struct bridge_pcm* p);
// Waits (at most 2 s) until written frames are presented, then stops.
void bridge_pcm_drain(struct bridge_pcm* p);
// Enters standby. The next read or write starts afresh.
void bridge_pcm_stop(struct bridge_pcm* p);
// Reports a playback stream's Android audio_usage_t when it differs from the
// last one sent, including across a bridge reconnect.
void bridge_pcm_set_usage(struct bridge_pcm* p, uint32_t usage);

#ifdef __cplusplus
}
#endif
