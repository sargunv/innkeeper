// The bridge's audio service. See linuxapk/audio.h.

#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include "linuxapk/audio.h"

#include "linuxapk/bridge_client.h"
#ifdef __ANDROID__
#include <android/log.h>
#define ALOGI(...) \
  __android_log_print(ANDROID_LOG_INFO, "linuxapk-audio", __VA_ARGS__)
#define ALOGW(...) \
  __android_log_print(ANDROID_LOG_WARN, "linuxapk-audio", __VA_ARGS__)
#define ALOGE(...) \
  __android_log_print(ANDROID_LOG_ERROR, "linuxapk-audio", __VA_ARGS__)
#else
#define ALOGI(...) ((void)0)
#define ALOGW(...) ((void)0)
#define ALOGE(...) ((void)0)
#endif

// Wire constants, mirrored from host/bridge/src/audio/{proto,ring}.rs.
#define T_OPEN_OUTPUT 0x0101
#define T_OPEN_INPUT 0x0107
#define T_CLOSE 0x0102
#define T_STANDBY 0x0103
#define T_SET_MASTER 0x0105

#define RING_MAGIC 0x5241584cu
#define OFF_WRITE_POS 64
#define OFF_READ_POS 128
#define OFF_TIMING_SEQ 192
#define OFF_PLAYED 200
#define OFF_TS_NS 208
#define OFF_STATE 224
#define DATA_OFFSET 4096
#define STATE_PLAYING 1
#define STATE_DEAD 2

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_sock = -1;
static uint32_t g_tag;
static float g_master_vol = 1.0f;
static int g_master_mute;

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

// --- connection (g_lock held) ---------------------------------------------

static void request(struct lxb_msg* m, uint16_t type) {
  lxb_msg_init(m, type, ++g_tag);
}

// Sends a request and waits for its reply. Returns its status, or -errno if
// the connection failed (and is now closed).
static int call_locked(const struct lxb_msg* m, struct lxb_packet* reply) {
  int r = lxb_call(g_sock, m, reply);
  if (r != 0) {
    ALOGE("bridge: no reply (%s)", strerror(-r));
    close(g_sock);
    g_sock = -1;
    return r;
  }
  return reply->status;
}

// call_locked for requests whose replies carry nothing.
static int call_status_locked(const struct lxb_msg* m) {
  struct lxb_packet reply;
  int st = call_locked(m, &reply);
  lxb_close_fds(&reply);
  return st;
}

static int set_master_locked(void) {
  struct lxb_msg m;
  request(&m, T_SET_MASTER);
  lxb_put_f32(&m, g_master_vol);
  lxb_put_u8(&m, (uint8_t)(g_master_mute != 0));
  return call_status_locked(&m);
}

static int connect_locked(void) {
  if (g_sock >= 0) return 0;
  char name[64];
  snprintf(name, sizeof(name), "tinyalsa pid %d", getpid());
  int s = lxb_connect(LXB_SERVICE_AUDIO, name);
  if (s < 0) {
    ALOGW("bridge: connect %s: %s", LXB_SOCKET_PATH, strerror(-s));
    return s;
  }
  g_sock = s;
  ALOGI("bridge: connected");
  if (g_master_vol != 1.0f || g_master_mute) set_master_locked();
  return 0;
}

// --- streams ---------------------------------------------------------------

static _Atomic uint64_t* hdr64(const struct lxb_stream* s, size_t off) {
  return (_Atomic uint64_t*)(s->map + off);
}
static _Atomic uint32_t* hdr32(const struct lxb_stream* s, size_t off) {
  return (_Atomic uint32_t*)(s->map + off);
}

static int open_stream(
  const struct lxb_audio_cfg* cfg, struct lxb_stream* s, bool input
) {
  memset(s, 0, sizeof(*s));
  s->data_fd = s->space_fd = -1;
  uint32_t sample = cfg->format == LXB_F32LE ? 4 : 2;
  pthread_mutex_lock(&g_lock);
  int r = connect_locked();
  if (r != 0) {
    pthread_mutex_unlock(&g_lock);
    return r;
  }
  struct lxb_msg req;
  request(&req, input ? T_OPEN_INPUT : T_OPEN_OUTPUT);
  lxb_put_u32(&req, cfg->rate);
  lxb_put_u8(&req, cfg->channels);
  lxb_put_u8(&req, cfg->format);
  lxb_put_u16(&req, 0);
  lxb_put_u32(&req, cfg->period_frames);
  lxb_put_u32(&req, cfg->periods);
  lxb_put_name(&req, cfg->name);
  struct lxb_packet reply;
  int st = call_locked(&req, &reply);
  pthread_mutex_unlock(&g_lock);
  if (st != 0 || reply.len < 20 || reply.nfds != 3) {
    lxb_close_fds(&reply);
    ALOGE("bridge: open stream failed (%d)", st);
    return st < 0 ? st : -EPROTO;
  }
  int* fds = reply.fds;
  uint32_t v[5];
  for (int i = 0; i < 5; i++) v[i] = lxb_u32(reply.body + 4 * i);
  s->id = v[0];
  s->capacity = v[1];
  s->latency_us = v[4];
  s->frame_bytes = cfg->channels * sample;
  s->rate = cfg->rate;
  s->map_size = v[3];
  struct stat st_buf;
  if (
    v[2] != DATA_OFFSET || s->capacity != cfg->period_frames * cfg->periods ||
    fstat(fds[0], &st_buf) != 0 || (size_t)st_buf.st_size < s->map_size ||
    s->map_size < DATA_OFFSET + (size_t)s->capacity * s->frame_bytes
  ) {
    ALOGE("bridge: implausible ring");
    for (int i = 0; i < 3; i++) close(fds[i]);
    return -EPROTO;
  }
  void* m =
    mmap(NULL, s->map_size, PROT_READ | PROT_WRITE, MAP_SHARED, fds[0], 0);
  close(fds[0]);
  if (
    m == MAP_FAILED || lxb_u32(m) != RING_MAGIC ||
    lxb_u32((uint8_t*)m + 4) != 1 ||
    lxb_u32((uint8_t*)m + 8) != s->frame_bytes ||
    lxb_u32((uint8_t*)m + 12) != s->capacity ||
    lxb_u32((uint8_t*)m + 16) != cfg->rate ||
    lxb_u32((uint8_t*)m + 20) != DATA_OFFSET
  ) {
    if (m != MAP_FAILED) munmap(m, s->map_size);
    close(fds[1]);
    close(fds[2]);
    return -EPROTO;
  }
  s->input = input;
  s->map = m;
  s->data_fd = fds[1];
  s->space_fd = fds[2];
  ALOGI(
    "bridge: stream #%u %u Hz, ring %u frames, downstream %u us", s->id,
    s->rate, s->capacity, s->latency_us
  );
  return 0;
}

int lxb_open_output(const struct lxb_audio_cfg* cfg, struct lxb_stream* s) {
  return open_stream(cfg, s, false);
}
int lxb_open_input(const struct lxb_audio_cfg* cfg, struct lxb_stream* s) {
  return open_stream(cfg, s, true);
}

static int stream_call(uint16_t type, uint32_t id) {
  pthread_mutex_lock(&g_lock);
  int st = -ENOTCONN;
  if (g_sock >= 0) {
    struct lxb_msg m;
    request(&m, type);
    lxb_put_u32(&m, id);
    st = call_status_locked(&m);
  }
  pthread_mutex_unlock(&g_lock);
  return st;
}

void lxb_close_stream(struct lxb_stream* s) {
  if (!s->map) return;
  stream_call(T_CLOSE, s->id);
  munmap(s->map, s->map_size);
  close(s->data_fd);
  close(s->space_fd);
  memset(s, 0, sizeof(*s));
  s->data_fd = s->space_fd = -1;
}

int lxb_standby(struct lxb_stream* s) {
  if (s->input) {
    int r = s->map ? stream_call(T_STANDBY, s->id) : -ENOTCONN;
    lxb_close_stream(s);
    return r;
  }
  return s->map ? stream_call(T_STANDBY, s->id) : -ENOTCONN;
}

int lxb_set_master(float volume, int muted) {
  if (!(volume >= 0.0f)) volume = 0.0f;  // also catches NaN
  if (volume > 1.0f) volume = 1.0f;
  pthread_mutex_lock(&g_lock);
  g_master_vol = volume;
  g_master_mute = muted != 0;
  int st = g_sock >= 0 ? set_master_locked() : -ENOTCONN;
  pthread_mutex_unlock(&g_lock);
  return st;
}

int lxb_dead(const struct lxb_stream* s) {
  return !s->map ||
         atomic_load_explicit(hdr32(s, OFF_STATE), memory_order_acquire) ==
           STATE_DEAD;
}

uint64_t lxb_consumed(const struct lxb_stream* s) {
  uint64_t r =
    atomic_load_explicit(hdr64(s, OFF_READ_POS), memory_order_acquire);
  uint64_t w = __atomic_load_n(&s->written, __ATOMIC_ACQUIRE);
  return r > w ? w : r;
}

int lxb_write(
  struct lxb_stream* s, const void* buf, uint32_t frames, int timeout_ms
) {
  const uint8_t* src = buf;
  uint32_t done = 0;
  int64_t deadline = now_ns() + (int64_t)timeout_ms * 1000000;
  uint8_t* data = s->map + DATA_OFFSET;
  while (done < frames) {
    if (lxb_dead(s)) return done ? (int)done : -EPIPE;
    uint64_t queued = s->written - lxb_consumed(s);
    uint64_t space = queued >= s->capacity ? 0 : s->capacity - queued;
    if (space == 0) {
      int64_t left = (deadline - now_ns()) / 1000000;
      if (left <= 0) break;
      struct pollfd p = {.fd = s->space_fd, .events = POLLIN};
      if (poll(&p, 1, (int)left) > 0) {
        uint64_t v;
        (void)!read(s->space_fd, &v, sizeof(v));
      }
      continue;
    }
    uint32_t n = frames - done;
    if (n > space) n = (uint32_t)space;
    uint32_t idx = (uint32_t)(s->written % s->capacity);
    uint32_t first = s->capacity - idx;
    if (first > n) first = n;
    memcpy(
      data + (size_t)idx * s->frame_bytes, src + (size_t)done * s->frame_bytes,
      (size_t)first * s->frame_bytes
    );
    memcpy(
      data, src + (size_t)(done + first) * s->frame_bytes,
      (size_t)(n - first) * s->frame_bytes
    );
    __atomic_store_n(&s->written, s->written + n, __ATOMIC_RELEASE);
    done += n;
    atomic_store_explicit(
      hdr64(s, OFF_WRITE_POS), s->written, memory_order_release
    );
    uint64_t one = 1;
    (void)!write(s->data_fd, &one, sizeof(one));
  }
  return (int)done;
}

uint64_t lxb_available(const struct lxb_stream* s) {
  uint64_t w =
    atomic_load_explicit(hdr64(s, OFF_WRITE_POS), memory_order_acquire);
  uint64_t n = w >= s->read ? w - s->read : 0;
  return n > s->capacity ? s->capacity : n;
}

int lxb_read(struct lxb_stream* s, void* buf, uint32_t frames, int timeout_ms) {
  uint8_t* dst = buf;
  uint32_t done = 0;
  int64_t deadline = now_ns() + (int64_t)timeout_ms * 1000000;
  if (lxb_dead(s)) return -EPIPE;
  uint64_t one = 1;
  (void)!write(s->data_fd, &one, sizeof(one));  // resume capture on first read
  while (done < frames) {
    if (lxb_dead(s)) return done ? (int)done : -EPIPE;
    uint64_t ready = lxb_available(s);
    if (!ready) {
      int64_t left = (deadline - now_ns()) / 1000000;
      if (left <= 0) break;
      struct pollfd p = {.fd = s->space_fd, .events = POLLIN};
      if (poll(&p, 1, (int)left) > 0) {
        uint8_t token[64];
        (void)!read(s->space_fd, token, sizeof(token));
        if (p.revents & (POLLHUP | POLLERR)) return done ? (int)done : -EPIPE;
      }
      continue;
    }
    uint32_t n = frames - done;
    if (n > ready) n = (uint32_t)ready;
    uint32_t idx = (uint32_t)(s->read % s->capacity);
    uint32_t first = s->capacity - idx;
    if (first > n) first = n;
    memcpy(
      dst + (size_t)done * s->frame_bytes,
      s->map + DATA_OFFSET + (size_t)idx * s->frame_bytes,
      (size_t)first * s->frame_bytes
    );
    memcpy(
      dst + (size_t)(done + first) * s->frame_bytes, s->map + DATA_OFFSET,
      (size_t)(n - first) * s->frame_bytes
    );
    s->read += n;
    done += n;
    atomic_store_explicit(
      hdr64(s, OFF_READ_POS), s->read, memory_order_release
    );
  }
  return (int)done;
}

int lxb_presented(
  const struct lxb_stream* s, uint64_t* frames, struct timespec* ts
) {
  uint64_t played = 0, t = 0;
  for (int i = 0; i < 100; i++) {
    uint64_t q1 =
      atomic_load_explicit(hdr64(s, OFF_TIMING_SEQ), memory_order_acquire);
    if (q1 & 1) continue;
    played = atomic_load_explicit(hdr64(s, OFF_PLAYED), memory_order_relaxed);
    t = atomic_load_explicit(hdr64(s, OFF_TS_NS), memory_order_relaxed);
    atomic_thread_fence(memory_order_acquire);
    if (
      atomic_load_explicit(hdr64(s, OFF_TIMING_SEQ), memory_order_relaxed) == q1
    )
      break;
  }
  if (t == 0) return -EAGAIN;
  int64_t now = now_ns();
  uint64_t p = played;
  if (
    atomic_load_explicit(hdr32(s, OFF_STATE), memory_order_acquire) ==
      STATE_PLAYING &&
    now > (int64_t)t
  ) {
    p += (uint64_t)(now - (int64_t)t) * s->rate / 1000000000ULL;
  }
  uint64_t c = lxb_consumed(s);
  if (p > c) p = c;
  *frames = p;
  ts->tv_sec = now / 1000000000;
  ts->tv_nsec = now % 1000000000;
  return 0;
}
