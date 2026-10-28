// tinyalsa PCM API for Linux.apk: one virtual card ("Linux.apk", card 0,
// device 0) whose playback goes to the host bridge through a shared ring
// (audio_service.c), and whose capture is silence for now.
//
// This replaces the image's libtinyalsav2.so, so the AOSP default AIDL audio
// HAL plays to the desktop without changes. See docs/audio.md.
//
// Behaviour notes:
// - The bridge stream is opened lazily on the first write, so the HAL's
//   capability probes (open/close at each rate) are free.
// - Writes block while the ring is full: the HAL is paced by the host's
//   audio clock. Without a bridge (not running, or it went away) writes are
//   paced by CLOCK_MONOTONIC and the audio is dropped; we retry once a
//   second.
// - There's no device fd: pcm_get_file_descriptor() and pcm_get_poll_fd()
//   return -1. Use pcm_wait() to wait for room.
// - Positions (pcm_get_htimestamp, pcm_get_delay) come from the bridge's
//   presentation estimate, which includes PipeWire's queue and the device.

#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <tinyalsa/pcm.h>
#include <unistd.h>

#include "audio_service.h"
#include "linuxapk/bridge_client.h"
#include "log.h"

#define API __attribute__((visibility("default")))

// ALSA constants (from <sound/asound.h>) for pcm_params masks.
#define SNDRV_PCM_ACCESS_RW_INTERLEAVED 3
#define SNDRV_PCM_FORMAT_S16_LE 2
#define SNDRV_PCM_FORMAT_FLOAT_LE 14
#define SNDRV_PCM_SUBFORMAT_STD 0

#define MIN_PERIOD 16u
#define MAX_PERIOD 16384u
#define MIN_PERIODS 2u
#define MAX_PERIODS 16u
#define NATIVE_RATE 48000u
// Extra room in the reported buffer for PipeWire's queue and the device, so
// "frames written - (buffer - avail)" stays exact (alsa_device_proxy).
#define DOWNSTREAM_HEADROOM_MS 200u
#define WRITE_TIMEOUT_MS 500
#define RING_PERIOD_MS 10u

struct pcm {
  unsigned int card, device, flags;
  struct pcm_config config;
  bool ready;
  char error[128];
  pthread_mutex_t lock;
  // Output.
  struct lxb_stream s;  // s.map != NULL while connected
  uint64_t base;        // frames written before the current ring
  uint64_t total;       // frames accepted since open (atomic access)
  bool fallback;        // no bridge: virtual clock from fb_t0/fb_f0
  int64_t fb_t0;
  uint64_t fb_f0;
  struct lxb_backoff retry;  // reopening the stream
  // Input (silence).
  int64_t in_t0;
  uint64_t in_frames;
  bool running;
};

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static void set_error(struct pcm* p, const char* fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(p->error, sizeof(p->error), fmt, ap);
  va_end(ap);
}

static unsigned int format_bytes(enum pcm_format f) {
  switch (f) {
    case PCM_FORMAT_S16_LE:
    case PCM_FORMAT_S16_BE:
      return 2;
    case PCM_FORMAT_S8:
      return 1;
    case PCM_FORMAT_S24_3LE:
    case PCM_FORMAT_S24_3BE:
      return 3;
    case PCM_FORMAT_INVALID:
    case PCM_FORMAT_MAX:
      return 0;
    default:
      return 4;
  }
}

API unsigned int pcm_format_to_bits(enum pcm_format format) {
  switch (format) {
    case PCM_FORMAT_S24_LE:
    case PCM_FORMAT_S24_BE:
      return 32;  // 24 bits in 32
    default:
      return format_bytes(format) * 8;
  }
}

static uint64_t total_of(const struct pcm* p) {
  return __atomic_load_n(&p->total, __ATOMIC_ACQUIRE);
}

static uint64_t buffer_frames(const struct pcm* p) {
  return (uint64_t)p->config.period_size * p->config.period_count;
}

// --- open / close --------------------------------------------------------

static bool fill_config(struct pcm* p, const struct pcm_config* c) {
  if (!c) {
    set_error(p, "no config");
    return false;
  }
  p->config = *c;
  if (c->format != PCM_FORMAT_S16_LE && c->format != PCM_FORMAT_FLOAT_LE) {
    set_error(p, "unsupported format %d (S16_LE and FLOAT_LE only)", c->format);
    return false;
  }
  if (c->channels < 1 || c->channels > 2) {
    set_error(p, "unsupported channel count %u", c->channels);
    return false;
  }
  if (c->rate < 8000 || c->rate > 192000) {
    set_error(p, "unsupported rate %u", c->rate);
    return false;
  }
  struct pcm_config* k = &p->config;
  if (k->period_size == 0) k->period_size = 1024;
  if (k->period_count == 0) k->period_count = 4;
  if (k->period_size < MIN_PERIOD) k->period_size = MIN_PERIOD;
  if (k->period_size > MAX_PERIOD) k->period_size = MAX_PERIOD;
  if (k->period_count < MIN_PERIODS) k->period_count = MIN_PERIODS;
  if (k->period_count > MAX_PERIODS) k->period_count = MAX_PERIODS;
  if (!k->start_threshold)
    k->start_threshold = k->period_size * k->period_count;
  if (!k->stop_threshold) k->stop_threshold = k->period_size * k->period_count;
  return true;
}

API struct pcm* pcm_open(
  unsigned int card, unsigned int device, unsigned int flags,
  const struct pcm_config* config
) {
  struct pcm* p = calloc(1, sizeof(*p));
  if (!p) return NULL;
  pthread_mutex_init(&p->lock, NULL);
  p->card = card;
  p->device = device;
  p->flags = flags;
  p->s.data_fd = p->s.space_fd = -1;
  if (card != 0 || device != 0) {
    set_error(
      p, "no device %u on card %u (Linux.apk has card 0 device 0)", device, card
    );
    return p;
  }
  p->ready = fill_config(p, config);
  return p;
}

API struct pcm* pcm_open_by_name(
  const char* name, unsigned int flags, const struct pcm_config* config
) {
  unsigned int card = UINT_MAX, device = UINT_MAX;
  if (!name || sscanf(name, "hw:%u,%u", &card, &device) != 2) {
    card = device = UINT_MAX;
  }
  return pcm_open(card, device, flags, config);
}

static void disconnect_locked(struct pcm* p) {
  if (p->s.map) {
    p->base += __atomic_load_n(&p->s.written, __ATOMIC_ACQUIRE);
    lxb_close_output(&p->s);
  }
}

API int pcm_close(struct pcm* p) {
  if (!p) return 0;
  pthread_mutex_lock(&p->lock);
  disconnect_locked(p);
  pthread_mutex_unlock(&p->lock);
  pthread_mutex_destroy(&p->lock);
  free(p);
  return 0;
}

API int pcm_is_ready(const struct pcm* p) { return p && p->ready; }
API const char* pcm_get_error(const struct pcm* p) {
  return p ? p->error : "no pcm";
}
API unsigned int pcm_get_channels(const struct pcm* p) {
  return p->config.channels;
}
API const struct pcm_config* pcm_get_config(const struct pcm* p) {
  return p ? &p->config : NULL;
}
API unsigned int pcm_get_rate(const struct pcm* p) { return p->config.rate; }
API enum pcm_format pcm_get_format(const struct pcm* p) {
  return p->config.format;
}
API unsigned int pcm_get_subdevice(const struct pcm* p) {
  (void)p;
  return 0;
}

API int pcm_get_file_descriptor(const struct pcm* p) {
  (void)p;
  return -1;
}
API int pcm_get_poll_fd(struct pcm* p) {
  (void)p;
  return -1;
}

API int pcm_set_config(struct pcm* p, const struct pcm_config* config) {
  if (!p) return -EFAULT;
  pthread_mutex_lock(&p->lock);
  int r = 0;
  if (p->s.map || total_of(p) || p->in_frames) {
    r = -EBUSY;
  } else {
    p->ready = fill_config(p, config);
    if (!p->ready) r = -EINVAL;
  }
  pthread_mutex_unlock(&p->lock);
  return r;
}

API unsigned int pcm_get_buffer_size(const struct pcm* p) {
  if (!p) return 0;
  uint64_t f = buffer_frames(p);
  if (!(p->flags & PCM_IN))
    f += (uint64_t)p->config.rate * DOWNSTREAM_HEADROOM_MS / 1000;
  return (unsigned int)f;
}

API unsigned int pcm_frames_to_bytes(const struct pcm* p, unsigned int frames) {
  return frames * p->config.channels * format_bytes(p->config.format);
}
API unsigned int pcm_bytes_to_frames(const struct pcm* p, unsigned int bytes) {
  unsigned int fb = p->config.channels * format_bytes(p->config.format);
  return fb ? bytes / fb : 0;
}

// --- output ------------------------------------------------------------------

// Frames presented as of now (all rings and fallback time), never more than
// written.
static uint64_t presented_locked(struct pcm* p) {
  uint64_t total = total_of(p);
  uint64_t pr;
  if (p->s.map) {
    uint64_t f;
    struct timespec ts;
    pr = p->base + (lxb_presented(&p->s, &f, &ts) == 0 ? f : 0);
  } else if (p->fallback) {
    pr = p->fb_f0 +
         (uint64_t)(now_ns() - p->fb_t0) * p->config.rate / 1000000000ULL;
  } else {
    pr = total;
  }
  return pr > total ? total : pr;
}

// The ring is our share of the latency, so keep it small whatever buffer
// the HAL asked for: two periods of at most RING_PERIOD_MS. The HAL's own
// period still sets how much each write carries (a write bigger than the
// ring simply blocks until the last part fits).
static uint32_t ring_period(const struct pcm* p) {
  uint32_t max = p->config.rate * RING_PERIOD_MS / 1000;
  uint32_t f = p->config.period_size < max ? p->config.period_size : max;
  return f < MIN_PERIOD ? MIN_PERIOD : f;
}

static void connect_locked(struct pcm* p) {
  if (p->s.map || lxb_backoff_ms(&p->retry) != 0) return;
  struct lxb_out_cfg c = {
    .rate = p->config.rate,
    .channels = (uint8_t)p->config.channels,
    .format = p->config.format == PCM_FORMAT_FLOAT_LE ? LXB_F32LE : LXB_S16LE,
    .period_frames = ring_period(p),
    .periods = 2,
    .name = "Android",
  };
  uint64_t pos = presented_locked(p);
  if (lxb_open_output(&c, &p->s) == 0) {
    // Whatever the virtual clock hadn't played yet is lost; start the
    // new ring's positions from here.
    p->base = total_of(p);
    p->fallback = false;
  } else if (!p->fallback) {
    p->fallback = true;
    p->fb_t0 = now_ns();
    p->fb_f0 = pos;
  }
}

static void enter_fallback_locked(struct pcm* p) {
  uint64_t pos = presented_locked(p);
  disconnect_locked(p);
  p->base = total_of(p);
  p->fallback = true;
  p->fb_t0 = now_ns();
  p->fb_f0 = pos;
}

// No bridge: block like a device consuming at the nominal rate.
static int fallback_write(struct pcm* p, unsigned int frames) {
  for (;;) {
    pthread_mutex_lock(&p->lock);
    uint64_t queued = total_of(p) - presented_locked(p);
    pthread_mutex_unlock(&p->lock);
    uint64_t cap = buffer_frames(p);
    if (queued + frames <= cap || queued == 0) break;
    uint64_t wait_ns = (queued + frames - cap) * 1000000000ULL / p->config.rate;
    struct timespec ts = {
      .tv_sec = (time_t)(wait_ns / 1000000000ULL),
      .tv_nsec = (long)(wait_ns % 1000000000ULL)
    };
    nanosleep(&ts, NULL);
  }
  __atomic_fetch_add(&p->total, frames, __ATOMIC_ACQ_REL);
  return (int)frames;
}

API int pcm_writei(struct pcm* p, const void* data, unsigned int frames) {
  if (!pcm_is_ready(p)) return -EBADFD;
  if (p->flags & PCM_IN) return -EINVAL;
  if (frames == 0) return 0;
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
  // Dead stream, or the bridge stopped consuming for WRITE_TIMEOUT_MS.
  ALOGW(
    "bridge stream %s; dropping to the virtual clock",
    r == 0 ? "stalled" : "died"
  );
  pthread_mutex_lock(&p->lock);
  enter_fallback_locked(p);
  pthread_mutex_unlock(&p->lock);
  return fallback_write(p, frames);
}

API int pcm_write(struct pcm* p, const void* data, unsigned int count) {
  if (!pcm_is_ready(p)) return -EBADFD;
  unsigned int frames = pcm_bytes_to_frames(p, count);
  const uint8_t* d = data;
  unsigned int done = 0;
  while (done < frames) {
    int r = pcm_writei(p, d + pcm_frames_to_bytes(p, done), frames - done);
    if (r < 0) return r;
    done += (unsigned int)r;
  }
  return 0;
}

API int pcm_mmap_write(struct pcm* p, const void* data, unsigned int count) {
  return pcm_write(p, data, count);
}

// --- input (silence, paced by the clock)
// --------------------------------------

API int pcm_readi(struct pcm* p, void* data, unsigned int frames) {
  if (!pcm_is_ready(p)) return -EBADFD;
  if (!(p->flags & PCM_IN)) return -EINVAL;
  if (!p->running || !p->in_t0) {
    p->in_t0 = now_ns();
    p->in_frames = 0;
    p->running = true;
  }
  uint64_t due_ns = (p->in_frames + frames) * 1000000000ULL / p->config.rate;
  int64_t wake = p->in_t0 + (int64_t)due_ns;
  int64_t t = now_ns();
  if (wake > t) {
    struct timespec ts = {
      .tv_sec = (time_t)(wake / 1000000000),
      .tv_nsec = (long)(wake % 1000000000)
    };
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL);
  }
  memset(data, 0, pcm_frames_to_bytes(p, frames));
  p->in_frames += frames;
  return (int)frames;
}

API int pcm_read(struct pcm* p, void* data, unsigned int count) {
  int r = pcm_readi(p, data, pcm_bytes_to_frames(p, count));
  return r < 0 ? r : 0;
}

API int pcm_mmap_read(struct pcm* p, void* data, unsigned int count) {
  return pcm_read(p, data, count);
}

// --- state and position
// --------------------------------------------------------

API int pcm_prepare(struct pcm* p) {
  if (!pcm_is_ready(p)) return -EBADFD;
  pthread_mutex_lock(&p->lock);
  if (p->s.map && lxb_dead(&p->s)) {
    enter_fallback_locked(p);
    p->retry = (struct lxb_backoff){0};
  }
  pthread_mutex_unlock(&p->lock);
  return 0;
}

API int pcm_start(struct pcm* p) {
  if (!pcm_is_ready(p)) return -EBADFD;
  p->running = true;
  if (p->flags & PCM_IN) {
    p->in_t0 = now_ns();
    p->in_frames = 0;
  }
  return 0;
}

API int pcm_stop(struct pcm* p) {
  if (!pcm_is_ready(p)) return -EBADFD;
  pthread_mutex_lock(&p->lock);
  if (p->s.map) lxb_standby(&p->s);
  p->running = false;
  p->in_t0 = 0;
  pthread_mutex_unlock(&p->lock);
  return 0;
}

API int pcm_drain(struct pcm* p) {
  if (!pcm_is_ready(p) || (p->flags & PCM_IN)) return -EBADFD;
  int64_t deadline = now_ns() + 2000000000LL;
  while (now_ns() < deadline) {
    pthread_mutex_lock(&p->lock);
    uint64_t queued = total_of(p) - presented_locked(p);
    pthread_mutex_unlock(&p->lock);
    if (queued == 0) break;
    struct timespec ts = {0, 5000000};
    nanosleep(&ts, NULL);
  }
  return pcm_stop(p);
}

API long pcm_get_delay(struct pcm* p) {
  if (!pcm_is_ready(p)) return -EBADFD;
  if (p->flags & PCM_IN) return 0;
  pthread_mutex_lock(&p->lock);
  uint64_t q = total_of(p) - presented_locked(p);
  pthread_mutex_unlock(&p->lock);
  return (long)q;
}

API int pcm_get_htimestamp(
  struct pcm* p, unsigned int* avail, struct timespec* tstamp
) {
  if (!pcm_is_ready(p)) return -1;
  clock_gettime(CLOCK_MONOTONIC, tstamp);
  uint64_t buf = pcm_get_buffer_size(p);
  if (p->flags & PCM_IN) {
    uint64_t ready = 0;
    if (p->running && p->in_t0) {
      uint64_t now_frames =
        (uint64_t)(now_ns() - p->in_t0) * p->config.rate / 1000000000ULL;
      ready = now_frames > p->in_frames ? now_frames - p->in_frames : 0;
    }
    *avail = (unsigned int)(ready > buf ? buf : ready);
    return 0;
  }
  pthread_mutex_lock(&p->lock);
  uint64_t q = total_of(p) - presented_locked(p);
  pthread_mutex_unlock(&p->lock);
  *avail = (unsigned int)(q >= buf ? 0 : buf - q);
  return 0;
}

API int pcm_wait(struct pcm* p, int timeout) {
  if (!pcm_is_ready(p)) return -EBADFD;
  if ((p->flags & PCM_IN) || !p->s.map) return 1;
  uint64_t queued =
    __atomic_load_n(&p->s.written, __ATOMIC_ACQUIRE) - lxb_consumed(&p->s);
  if (queued < p->s.capacity) return 1;
  struct pollfd fd = {.fd = p->s.space_fd, .events = POLLIN};
  int r = poll(&fd, 1, timeout);
  if (r > 0) {
    uint64_t v;
    (void)!read(p->s.space_fd, &v, sizeof(v));
  }
  return r < 0 ? -errno : r;
}

// --- not supported: mmap, linking, raw ioctls
// -------------------------------------

API int pcm_mmap_begin(
  struct pcm* p, void** areas, unsigned int* offset, unsigned int* frames
) {
  (void)p, (void)areas, (void)offset, (void)frames;
  return -ENOSYS;
}
API int pcm_mmap_commit(
  struct pcm* p, unsigned int offset, unsigned int frames
) {
  (void)p, (void)offset, (void)frames;
  return -ENOSYS;
}
API int pcm_mmap_avail(struct pcm* p) {
  unsigned int avail;
  struct timespec ts;
  return pcm_get_htimestamp(p, &avail, &ts) == 0 ? (int)avail : -EBADFD;
}
API int pcm_mmap_get_hw_ptr(
  struct pcm* p, unsigned int* hw_ptr, struct timespec* tstamp
) {
  if (!pcm_is_ready(p)) return -EBADFD;
  pthread_mutex_lock(&p->lock);
  *hw_ptr = (unsigned int)presented_locked(p);
  pthread_mutex_unlock(&p->lock);
  clock_gettime(CLOCK_MONOTONIC, tstamp);
  return 0;
}
API int pcm_link(struct pcm* a, struct pcm* b) {
  (void)a, (void)b;
  return -ENOSYS;
}
API int pcm_unlink(struct pcm* p) {
  (void)p;
  return -ENOSYS;
}
API int pcm_ioctl(struct pcm* p, int code, ...) {
  (void)p, (void)code;
  return -ENOSYS;
}

// --- capabilities
// ----------------------------------------------------------------

struct pcm_params {
  struct pcm_mask masks[3];       // ACCESS, FORMAT, SUBFORMAT
  unsigned int min[12], max[12];  // SAMPLE_BITS .. TICK_TIME
};

static void mask_set(struct pcm_mask* m, unsigned int bit) {
  m->bits[bit / (sizeof(m->bits[0]) * 8)] |=
    1u << (bit % (sizeof(m->bits[0]) * 8));
}

API struct pcm_params* pcm_params_get(
  unsigned int card, unsigned int device, unsigned int flags
) {
  (void)flags;
  if (card != 0 || device != 0) return NULL;
  struct pcm_params* p = calloc(1, sizeof(*p));
  if (!p) return NULL;
  mask_set(&p->masks[PCM_PARAM_ACCESS], SNDRV_PCM_ACCESS_RW_INTERLEAVED);
  mask_set(&p->masks[PCM_PARAM_FORMAT], SNDRV_PCM_FORMAT_S16_LE);
  mask_set(&p->masks[PCM_PARAM_FORMAT], SNDRV_PCM_FORMAT_FLOAT_LE);
  mask_set(&p->masks[PCM_PARAM_SUBFORMAT], SNDRV_PCM_SUBFORMAT_STD);
#define IV(param, lo, hi)                          \
  (p->min[(param) - PCM_PARAM_SAMPLE_BITS] = (lo), \
   p->max[(param) - PCM_PARAM_SAMPLE_BITS] = (hi))
  IV(PCM_PARAM_SAMPLE_BITS, 16, 32);
  IV(PCM_PARAM_FRAME_BITS, 16, 64);
  IV(PCM_PARAM_CHANNELS, 1, 2);
  IV(PCM_PARAM_RATE, NATIVE_RATE, NATIVE_RATE);
  IV(
    PCM_PARAM_PERIOD_TIME, MIN_PERIOD * 1000000u / NATIVE_RATE,
    MAX_PERIOD * 1000u / (NATIVE_RATE / 1000)
  );
  IV(PCM_PARAM_PERIOD_SIZE, MIN_PERIOD, MAX_PERIOD);
  IV(PCM_PARAM_PERIOD_BYTES, MIN_PERIOD * 2, MAX_PERIOD * 8);
  IV(PCM_PARAM_PERIODS, MIN_PERIODS, MAX_PERIODS);
  IV(
    PCM_PARAM_BUFFER_TIME, MIN_PERIOD * MIN_PERIODS * 1000000u / NATIVE_RATE,
    MAX_PERIOD * MAX_PERIODS * 1000u / (NATIVE_RATE / 1000)
  );
  IV(PCM_PARAM_BUFFER_SIZE, MIN_PERIOD * MIN_PERIODS, MAX_PERIOD * MAX_PERIODS);
  IV(
    PCM_PARAM_BUFFER_BYTES, MIN_PERIOD * MIN_PERIODS * 2,
    MAX_PERIOD * MAX_PERIODS * 8
  );
  IV(PCM_PARAM_TICK_TIME, 0, 0);
#undef IV
  return p;
}

API void pcm_params_free(struct pcm_params* p) { free(p); }

API const struct pcm_mask* pcm_params_get_mask(
  const struct pcm_params* p, enum pcm_param param
) {
  if (!p || param < PCM_PARAM_ACCESS || param > PCM_PARAM_SUBFORMAT)
    return NULL;
  return &p->masks[param];
}

API unsigned int pcm_params_get_min(
  const struct pcm_params* p, enum pcm_param param
) {
  if (!p || param < PCM_PARAM_SAMPLE_BITS || param > PCM_PARAM_TICK_TIME)
    return 0;
  return p->min[param - PCM_PARAM_SAMPLE_BITS];
}

API unsigned int pcm_params_get_max(
  const struct pcm_params* p, enum pcm_param param
) {
  if (!p || param < PCM_PARAM_SAMPLE_BITS || param > PCM_PARAM_TICK_TIME)
    return 0;
  return p->max[param - PCM_PARAM_SAMPLE_BITS];
}

API int pcm_params_format_test(struct pcm_params* p, enum pcm_format format) {
  (void)p;
  return format == PCM_FORMAT_S16_LE || format == PCM_FORMAT_FLOAT_LE;
}

API int pcm_params_to_string(
  struct pcm_params* p, char* string, unsigned int size
) {
  if (!p || !string || size == 0) return 0;
  return snprintf(
    string, size,
    "Linux.apk: formats S16_LE FLOAT_LE, channels %u-%u, rate %u-%u, period "
    "%u-%u x %u-%u\n",
    pcm_params_get_min(p, PCM_PARAM_CHANNELS),
    pcm_params_get_max(p, PCM_PARAM_CHANNELS),
    pcm_params_get_min(p, PCM_PARAM_RATE),
    pcm_params_get_max(p, PCM_PARAM_RATE),
    pcm_params_get_min(p, PCM_PARAM_PERIOD_SIZE),
    pcm_params_get_max(p, PCM_PARAM_PERIOD_SIZE),
    pcm_params_get_min(p, PCM_PARAM_PERIODS),
    pcm_params_get_max(p, PCM_PARAM_PERIODS)
  );
}
