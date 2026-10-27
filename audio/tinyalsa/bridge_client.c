// Linux.apk bridge client (protocol v1, docs/bridge.md). See bridge_client.h.

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdbool.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include "bridge_client.h"

#include "log.h"

// Wire constants, mirrored from bridge/src/proto.rs and ring.rs.
#define T_HELLO 0x0001
#define T_OPEN_OUTPUT 0x0101
#define T_CLOSE 0x0102
#define T_STANDBY 0x0103
#define T_SET_VOLUME 0x0104
#define T_SET_MASTER 0x0105
#define T_REPLY 0x8000
#define MAX_MSG 512
#define MAX_NAME 96
#define SERVICE_AUDIO 1

#define RING_MAGIC 0x5241584cu
#define OFF_WRITE_POS 64
#define OFF_READ_POS 128
#define OFF_TIMING_SEQ 192
#define OFF_PLAYED 200
#define OFF_TS_NS 208
#define OFF_LATENCY_NS 216
#define OFF_STATE 224
#define DATA_OFFSET 4096
#define STATE_PLAYING 1
#define STATE_DEAD 2

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_sock = -1;
static uint32_t g_tag;
static float g_master_vol = 1.0f;
static int g_master_mute;
static int64_t g_next_attempt_ns;

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

// --- message building ------------------------------------------------------

struct wr {
  uint8_t b[MAX_MSG];
  size_t n;
};

static void put(struct wr* w, const void* p, size_t n) {
  if (w->n + n <= sizeof(w->b)) {
    memcpy(w->b + w->n, p, n);
    w->n += n;
  }
}
static void put_u8(struct wr* w, uint8_t v) { put(w, &v, 1); }
static void put_u16(struct wr* w, uint16_t v) {
  put(w, &v, 2);
}  // x86_64/arm64: LE
static void put_u32(struct wr* w, uint32_t v) { put(w, &v, 4); }
static void put_f32(struct wr* w, float v) { put(w, &v, 4); }
static void put_name(struct wr* w, const char* s) {
  size_t len = s ? strlen(s) : 0;
  size_t n = len > MAX_NAME ? MAX_NAME : len;
  // Don't split a UTF-8 sequence.
  while (n > 0 && n < len && ((uint8_t)s[n] & 0xc0) == 0x80) n--;
  put_u16(w, (uint16_t)n);
  put(w, s, n);
}
static void header(struct wr* w, uint16_t type) {
  w->n = 0;
  put_u16(w, type);
  put_u16(w, 0);
  put_u32(w, ++g_tag);
}

// --- connection (g_lock held) ---------------------------------------------

static void disconnect_locked(void) {
  if (g_sock >= 0) close(g_sock);
  g_sock = -1;
}

// Sends a request and waits for its reply. On success returns the status
// and fills body (up to *body_len) and up to 3 fds (others are closed).
static int call_locked(
  const struct wr* w, uint8_t* body, size_t* body_len, int* fds, int* nfds
) {
  if (send(g_sock, w->b, w->n, MSG_NOSIGNAL) != (ssize_t)w->n) {
    disconnect_locked();
    return -EPIPE;
  }
  uint32_t tag;
  memcpy(&tag, w->b + 4, 4);
  for (;;) {
    struct pollfd p = {.fd = g_sock, .events = POLLIN};
    int pr = poll(&p, 1, 3000);
    if (pr <= 0) {
      ALOGE("bridge: no reply (%s)", pr == 0 ? "timeout" : strerror(errno));
      disconnect_locked();
      return -ETIMEDOUT;
    }
    uint8_t buf[MAX_MSG];
    union {
      struct cmsghdr h;
      char space[CMSG_SPACE(sizeof(int) * 4)];
    } ctl;
    struct iovec iov = {.iov_base = buf, .iov_len = sizeof(buf)};
    struct msghdr m = {
      .msg_iov = &iov,
      .msg_iovlen = 1,
      .msg_control = &ctl,
      .msg_controllen = sizeof(ctl)
    };
    ssize_t n = recvmsg(g_sock, &m, MSG_CMSG_CLOEXEC);
    int got[4];
    int ngot = 0;
    for (struct cmsghdr* c = CMSG_FIRSTHDR(&m); c; c = CMSG_NXTHDR(&m, c)) {
      if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
        int k = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
        for (int i = 0; i < k && ngot < 4; i++)
          memcpy(&got[ngot++], CMSG_DATA(c) + i * sizeof(int), sizeof(int));
      }
    }
    if (n < 12) {
      for (int i = 0; i < ngot; i++) close(got[i]);
      disconnect_locked();
      return -EPIPE;
    }
    uint16_t type;
    uint32_t rtag;
    int32_t status;
    memcpy(&type, buf, 2);
    memcpy(&rtag, buf + 4, 4);
    memcpy(&status, buf + 8, 4);
    if (type != T_REPLY || rtag != tag) {
      for (int i = 0; i < ngot; i++) close(got[i]);
      continue;  // not ours (can't happen with one caller at a time)
    }
    size_t blen = (size_t)n - 12;
    if (body_len) {
      if (blen > *body_len) blen = *body_len;
      memcpy(body, buf + 12, blen);
      *body_len = blen;
    }
    int keep = nfds ? *nfds : 0;
    for (int i = 0; i < ngot; i++) {
      if (i < keep)
        fds[i] = got[i];
      else
        close(got[i]);
    }
    if (nfds) *nfds = ngot < keep ? ngot : keep;
    return status;
  }
}

static int connect_locked(void) {
  if (g_sock >= 0) return 0;
  // Don't hammer a missing bridge: one attempt per second.
  int64_t t = now_ns();
  if (t < g_next_attempt_ns) return -ECONNREFUSED;
  g_next_attempt_ns = t + 1000000000LL;
  int s = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
  if (s < 0) return -errno;
  struct sockaddr_un a = {.sun_family = AF_UNIX};
  strncpy(a.sun_path, LXB_SOCKET_PATH, sizeof(a.sun_path) - 1);
  if (connect(s, (struct sockaddr*)&a, sizeof(a)) != 0) {
    int e = errno;
    ALOGW("bridge: connect %s: %s", LXB_SOCKET_PATH, strerror(e));
    close(s);
    return -e;
  }
  g_sock = s;
  struct wr w;
  header(&w, T_HELLO);
  put_u16(&w, 1);  // major
  put_u16(&w, 0);  // minor
  put_u32(&w, SERVICE_AUDIO);
  char name[64];
  snprintf(name, sizeof(name), "tinyalsa pid %d", getpid());
  put_name(&w, name);
  int st = call_locked(&w, NULL, NULL, NULL, NULL);
  if (st != 0) {
    ALOGE("bridge: HELLO refused (%d)", st);
    disconnect_locked();
    return st < 0 ? st : -EPROTO;
  }
  ALOGI("bridge: connected");
  if (g_master_vol != 1.0f || g_master_mute) {
    header(&w, T_SET_MASTER);
    put_f32(&w, g_master_vol);
    put_u8(&w, (uint8_t)(g_master_mute != 0));
    call_locked(&w, NULL, NULL, NULL, NULL);
  }
  return 0;
}

// --- streams ---------------------------------------------------------------

static _Atomic uint64_t* hdr64(const struct lxb_stream* s, size_t off) {
  return (_Atomic uint64_t*)(s->map + off);
}
static _Atomic uint32_t* hdr32(const struct lxb_stream* s, size_t off) {
  return (_Atomic uint32_t*)(s->map + off);
}

int lxb_open_output(const struct lxb_out_cfg* cfg, struct lxb_stream* s) {
  memset(s, 0, sizeof(*s));
  s->data_fd = s->space_fd = -1;
  uint32_t sample = cfg->format == LXB_F32LE ? 4 : 2;
  pthread_mutex_lock(&g_lock);
  int r = connect_locked();
  if (r != 0) {
    pthread_mutex_unlock(&g_lock);
    return r;
  }
  struct wr w;
  header(&w, T_OPEN_OUTPUT);
  put_u32(&w, cfg->rate);
  put_u8(&w, cfg->channels);
  put_u8(&w, cfg->format);
  put_u16(&w, 0);
  put_u32(&w, cfg->period_frames);
  put_u32(&w, cfg->periods);
  put_name(&w, cfg->name);
  uint8_t body[64];
  size_t blen = sizeof(body);
  int fds[3] = {-1, -1, -1};
  int nfds = 3;
  int st = call_locked(&w, body, &blen, fds, &nfds);
  pthread_mutex_unlock(&g_lock);
  if (st != 0 || blen < 20 || nfds != 3) {
    for (int i = 0; i < nfds; i++) close(fds[i]);
    ALOGE("bridge: open output failed (%d)", st);
    return st < 0 ? st : -EPROTO;
  }
  uint32_t v[5];
  memcpy(v, body, sizeof(v));
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
  if (m == MAP_FAILED || *(uint32_t*)m != RING_MAGIC) {
    if (m != MAP_FAILED) munmap(m, s->map_size);
    close(fds[1]);
    close(fds[2]);
    return -EPROTO;
  }
  s->map = m;
  s->data_fd = fds[1];
  s->space_fd = fds[2];
  ALOGI(
    "bridge: output #%u %u Hz, ring %u frames, downstream %u us", s->id,
    s->rate, s->capacity, s->latency_us
  );
  return 0;
}

static int simple_call(uint16_t type, uint32_t id, const float* f, int nf) {
  pthread_mutex_lock(&g_lock);
  if (g_sock < 0) {
    pthread_mutex_unlock(&g_lock);
    return -ENOTCONN;
  }
  struct wr w;
  header(&w, type);
  put_u32(&w, id);
  for (int i = 0; i < nf; i++) put_f32(&w, f[i]);
  int st = call_locked(&w, NULL, NULL, NULL, NULL);
  pthread_mutex_unlock(&g_lock);
  return st;
}

void lxb_close_output(struct lxb_stream* s) {
  if (!s->map) return;
  simple_call(T_CLOSE, s->id, NULL, 0);
  munmap(s->map, s->map_size);
  close(s->data_fd);
  close(s->space_fd);
  memset(s, 0, sizeof(*s));
  s->data_fd = s->space_fd = -1;
}

int lxb_standby(struct lxb_stream* s) {
  return s->map ? simple_call(T_STANDBY, s->id, NULL, 0) : -ENOTCONN;
}

int lxb_set_volume(struct lxb_stream* s, float left, float right) {
  float f[2] = {left, right};
  return s->map ? simple_call(T_SET_VOLUME, s->id, f, 2) : -ENOTCONN;
}

int lxb_set_master(float volume, int muted) {
  if (!(volume >= 0.0f)) volume = 0.0f;  // also catches NaN
  if (volume > 1.0f) volume = 1.0f;
  pthread_mutex_lock(&g_lock);
  g_master_vol = volume;
  g_master_mute = muted != 0;
  int st = -ENOTCONN;
  if (g_sock >= 0) {
    struct wr w;
    header(&w, T_SET_MASTER);
    put_f32(&w, volume);
    put_u8(&w, (uint8_t)(muted != 0));
    st = call_locked(&w, NULL, NULL, NULL, NULL);
  }
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

uint64_t lxb_latency_ns(const struct lxb_stream* s) {
  return atomic_load_explicit(hdr64(s, OFF_LATENCY_NS), memory_order_relaxed);
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
