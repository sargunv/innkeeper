// SPDX-License-Identifier: AGPL-3.0-or-later
// pcm.h over the bridge's shared rings (audio.c).

#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "innkeeper/pcm.h"

#include "innkeeper/audio_client.h"
#include "innkeeper/bridge_client.h"
#include "log.h"

#define FRAME_SAMPLE_BYTES 2u  // S16LE
#define WRITE_TIMEOUT_MS 500
#define RING_PERIOD_MS 10u

struct bridge_pcm {
  bool input;
  uint32_t rate, channels, period_frames, periods;
  const char* name;
  pthread_mutex_t lock;
  // Output.
  struct lxb_stream s;  // s.map != NULL while connected
  uint64_t base;        // frames written before the current ring
  uint64_t total;       // frames accepted since open (atomic access)
  bool fallback;        // no bridge: virtual clock from fb_t0/fb_f0
  int64_t fb_t0;
  uint64_t fb_f0;
  struct lxb_backoff retry;  // reopening the stream
  bool usage_set;            // a playback usage has been reported
  uint32_t usage;
  // Input fallback clock.
  int64_t in_t0;
  uint64_t in_frames;
  bool running;
};

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static uint32_t frame_bytes(const struct bridge_pcm* p) {
  return p->channels * FRAME_SAMPLE_BYTES;
}

static uint64_t total_of(const struct bridge_pcm* p) {
  return __atomic_load_n(&p->total, __ATOMIC_ACQUIRE);
}

static uint64_t buffer_frames(const struct bridge_pcm* p) {
  return (uint64_t)p->period_frames * p->periods;
}

struct bridge_pcm* bridge_pcm_open(
  bool input, uint32_t rate, uint32_t channels, uint32_t period_frames,
  uint32_t periods, const char* name
) {
  if (
    channels < 1 || channels > 2 || rate < 8000 || rate > 192000 ||
    period_frames < 16 || period_frames > 16384 || periods < 2 || periods > 16
  )
    return NULL;
  struct bridge_pcm* p = calloc(1, sizeof(*p));
  if (!p) return NULL;
  pthread_mutex_init(&p->lock, NULL);
  p->input = input;
  p->rate = rate;
  p->channels = channels;
  p->period_frames = period_frames;
  p->periods = periods;
  p->name = name;
  p->s.data_fd = p->s.space_fd = -1;
  return p;
}

static void disconnect_locked(struct bridge_pcm* p) {
  if (p->s.map) {
    p->base += __atomic_load_n(&p->s.written, __ATOMIC_ACQUIRE);
    lxb_close_stream(&p->s);
  }
}

void bridge_pcm_close(struct bridge_pcm* p) {
  if (!p) return;
  pthread_mutex_lock(&p->lock);
  disconnect_locked(p);
  pthread_mutex_unlock(&p->lock);
  pthread_mutex_destroy(&p->lock);
  free(p);
}

// Capture streams skip this because the bridge drops SET_METADATA on record
// streams.
void bridge_pcm_set_usage(struct bridge_pcm* p, uint32_t usage) {
  if (!p || p->input) return;
  pthread_mutex_lock(&p->lock);
  if (!p->usage_set || p->usage != usage) {
    p->usage = usage;
    p->usage_set = true;
    if (p->s.map) lxb_set_metadata(&p->s, usage, "");
  }
  pthread_mutex_unlock(&p->lock);
}

// ---- output ----

// Frames presented so far across all rings and fallback time, capped at the
// frames written.
static uint64_t presented_locked(struct bridge_pcm* p) {
  uint64_t total = total_of(p);
  uint64_t pr;
  if (p->s.map) {
    uint64_t f;
    struct timespec ts;
    pr = p->base + (lxb_presented(&p->s, &f, &ts) == 0 ? f : 0);
  } else if (p->fallback) {
    pr = p->fb_f0 + (uint64_t)(now_ns() - p->fb_t0) * p->rate / 1000000000ULL;
  } else {
    pr = total;
  }
  return pr > total ? total : pr;
}

// The ring adds to the output latency, so it stays at two periods of at most
// RING_PERIOD_MS whatever buffer the stream asked for. A write bigger than the
// ring blocks until its last part fits.
static uint32_t ring_period(const struct bridge_pcm* p) {
  uint32_t max = p->rate * RING_PERIOD_MS / 1000;
  uint32_t f = p->period_frames < max ? p->period_frames : max;
  return f < 16 ? 16 : f;
}

static void connect_locked(struct bridge_pcm* p) {
  if (p->s.map || lxb_backoff_ms(&p->retry) != 0) return;
  struct lxb_audio_cfg c = {
    .rate = p->rate,
    .channels = (uint8_t)p->channels,
    .format = LXB_S16LE,
    // Capture cannot backpressure the source, so the ring keeps the stream's
    // whole buffer and a burst read loses no samples while its consumer is
    // scheduled out.
    .period_frames = p->input ? p->period_frames : ring_period(p),
    .periods = p->input ? p->periods : 2,
    .name = p->name,
  };
  if (p->input) {
    lxb_open_input(&c, &p->s);
    return;
  }
  uint64_t pos = presented_locked(p);
  if (lxb_open_output(&c, &p->s) == 0) {
    // Frames the virtual clock had not played yet are lost, and the new
    // ring's positions start from here.
    p->base = total_of(p);
    p->fallback = false;
    // A reopened ring starts with the stream's name, so restore the last
    // reported usage.
    if (p->usage_set) lxb_set_metadata(&p->s, p->usage, "");
  } else if (!p->fallback) {
    p->fallback = true;
    p->fb_t0 = now_ns();
    p->fb_f0 = pos;
  }
}

static void enter_fallback_locked(struct bridge_pcm* p) {
  uint64_t pos = presented_locked(p);
  disconnect_locked(p);
  p->base = total_of(p);
  p->fallback = true;
  p->fb_t0 = now_ns();
  p->fb_f0 = pos;
}

// Without a bridge, block like a device that consumes at the nominal rate.
static int fallback_write(struct bridge_pcm* p, uint32_t frames) {
  for (;;) {
    pthread_mutex_lock(&p->lock);
    uint64_t queued = total_of(p) - presented_locked(p);
    pthread_mutex_unlock(&p->lock);
    uint64_t cap = buffer_frames(p);
    if (queued + frames <= cap || queued == 0) break;
    uint64_t wait_ns = (queued + frames - cap) * 1000000000ULL / p->rate;
    struct timespec ts = {
      .tv_sec = (time_t)(wait_ns / 1000000000ULL),
      .tv_nsec = (long)(wait_ns % 1000000000ULL)
    };
    nanosleep(&ts, NULL);
  }
  __atomic_fetch_add(&p->total, frames, __ATOMIC_ACQ_REL);
  return (int)frames;
}

static int write_frames(
  struct bridge_pcm* p, const void* data, uint32_t frames
) {
  pthread_mutex_lock(&p->lock);
  if (p->s.map && lxb_dead(&p->s)) {
    ALOGW("bridge dropped the stream; reconnecting");
    enter_fallback_locked(p);
    p->retry = (struct lxb_backoff){0};
  }
  connect_locked(p);
  bool connected = p->s.map != NULL;
  p->running = true;
  pthread_mutex_unlock(&p->lock);
  if (!connected) return fallback_write(p, frames);

  int r = lxb_write(&p->s, data, frames, WRITE_TIMEOUT_MS);
  if (r > 0) {
    __atomic_fetch_add(&p->total, (uint64_t)r, __ATOMIC_ACQ_REL);
    return r;
  }
  // The stream died, or the bridge stopped consuming for WRITE_TIMEOUT_MS.
  ALOGW(
    "bridge stream %s; dropping to the virtual clock",
    r == 0 ? "stalled" : "died"
  );
  pthread_mutex_lock(&p->lock);
  enter_fallback_locked(p);
  pthread_mutex_unlock(&p->lock);
  return fallback_write(p, frames);
}

int bridge_pcm_write(struct bridge_pcm* p, const void* data, uint32_t bytes) {
  if (!p || p->input) return -EINVAL;
  uint32_t frames = bytes / frame_bytes(p);
  const uint8_t* d = data;
  uint32_t done = 0;
  while (done < frames) {
    int r = write_frames(p, d + done * frame_bytes(p), frames - done);
    if (r < 0) return r;
    done += (uint32_t)r;
  }
  return 0;
}

// ---- input (bridge capture, or paced silence) ----

int bridge_pcm_read(struct bridge_pcm* p, void* data, uint32_t bytes) {
  if (!p || !p->input) return -EINVAL;
  uint32_t frames = bytes / frame_bytes(p);
  if (frames == 0) return 0;
  int64_t read_start = now_ns();
  if (!p->running || !p->in_t0) {
    p->in_t0 = now_ns();
    p->in_frames = 0;
    p->running = true;
  }
  pthread_mutex_lock(&p->lock);
  if (p->s.map && lxb_dead(&p->s)) {
    disconnect_locked(p);
    p->retry = (struct lxb_backoff){0};
  }
  connect_locked(p);
  int got = p->s.map ? lxb_read(
                         &p->s, data, frames,
                         (int)((uint64_t)frames * 1000 / p->rate) + 50
                       )
                     : 0;
  if (got < 0) got = 0;
  if (p->s.map && ((uint32_t)got < frames)) disconnect_locked(p);
  pthread_mutex_unlock(&p->lock);
  if ((uint32_t)got == frames) {
    p->in_t0 = now_ns();
    p->in_frames = 0;
    return 0;
  }
  uint64_t due_ns = (p->in_frames + frames) * 1000000000ULL / p->rate;
  int64_t wake = p->in_t0 + (int64_t)due_ns;
  // A slow reopen must not be followed by a burst of unpaced silence.
  int64_t earliest =
    read_start + (int64_t)((uint64_t)frames * 1000000000ULL / p->rate);
  if (wake < earliest) wake = earliest;
  int64_t t = now_ns();
  if (wake > t) {
    struct timespec ts = {
      .tv_sec = (time_t)(wake / 1000000000),
      .tv_nsec = (long)(wake % 1000000000)
    };
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL);
  }
  memset(
    (uint8_t*)data + (uint32_t)got * frame_bytes(p), 0,
    (frames - (uint32_t)got) * frame_bytes(p)
  );
  p->in_frames += frames;
  return 0;
}

// ---- state and position ----

void bridge_pcm_stop(struct bridge_pcm* p) {
  if (!p) return;
  pthread_mutex_lock(&p->lock);
  if (p->s.map) lxb_standby(&p->s);
  p->running = false;
  p->in_t0 = 0;
  if (p->input) p->retry = (struct lxb_backoff){0};
  pthread_mutex_unlock(&p->lock);
}

void bridge_pcm_drain(struct bridge_pcm* p) {
  if (!p || p->input) return;
  int64_t deadline = now_ns() + 2000000000LL;
  while (now_ns() < deadline) {
    pthread_mutex_lock(&p->lock);
    uint64_t queued = total_of(p) - presented_locked(p);
    pthread_mutex_unlock(&p->lock);
    if (queued == 0) break;
    struct timespec ts = {0, 5000000};
    nanosleep(&ts, NULL);
  }
  bridge_pcm_stop(p);
}

long bridge_pcm_delay(struct bridge_pcm* p) {
  if (!p || p->input) return 0;
  pthread_mutex_lock(&p->lock);
  uint64_t q = total_of(p) - presented_locked(p);
  pthread_mutex_unlock(&p->lock);
  return (long)q;
}
