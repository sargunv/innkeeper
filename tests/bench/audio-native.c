// SPDX-License-Identifier: AGPL-3.0-or-later
// Stable libpulse-simple C ABI, loaded at runtime like the desktop bridge.
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

typedef struct pa_simple pa_simple;
typedef struct {
  int format;
  uint32_t rate;
  uint8_t channels;
} sample_spec;
static pa_simple* (*simple_new)(
  const char*, const char*, int, const char*, const char*, const sample_spec*,
  const void*, const void*, int*
);
static int (*simple_read)(pa_simple*, void*, size_t, int*);
static int (*simple_write)(pa_simple*, const void*, size_t, int*);
static uint64_t (*simple_get_latency)(pa_simple*, int*);
static void (*simple_free)(pa_simple*);
typedef struct {
  uint32_t maxlength, tlength, prebuf, minreq, fragsize;
} buffer_attr;
static pa_simple* capture;
static uint64_t detected;
static int16_t marker[127];
static uint64_t now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000000000 + t.tv_nsec;
}
static void* record(void* unused) {
  (void)unused;
  int16_t ring[127] = {0};
  size_t index = 0;
  uint64_t end = now() + 4000000000;
  while (now() < end) {
    int16_t batch[480];
    if (simple_read(capture, batch, sizeof(batch), NULL) < 0) return NULL;
    uint64_t at = now();
    for (int i = 0; i < 480; i++) {
      ring[index++ % 127] = batch[i];
      if (index < 127) continue;
      double dot = 0, energy = 0;
      for (size_t j = 0; j < 127; j++) {
        double v = ring[(index + j) % 127];
        dot += v * (marker[j] > 0 ? 1 : -1);
        energy += v * v;
      }
      // Normalized matched filter; silence and unrelated signals cannot pass.
      if (energy > 1e8 && dot > 0 && dot * dot > 0.9 * 127 * energy) {
        // Read-return is an upper bound, including capture
        // buffering/scheduling.
        detected = at;
        return NULL;
      }
    }
  }
  return NULL;
}
int main(int argc, char** argv) {
  if (argc != 2) return 2;
  void* lib = dlopen("libpulse-simple.so.0", RTLD_NOW);
  if (!lib) return 3;
#define LOAD(name)                                            \
  *(void**)(&simple_##name) = dlsym(lib, "pa_simple_" #name); \
  if (!simple_##name) return 3
  LOAD(new);
  LOAD(read);
  LOAD(write);
  LOAD(get_latency);
  LOAD(free);
  sample_spec spec = {3, 48000, 1};  // PA_SAMPLE_S16LE
  char monitor[256];
  snprintf(monitor, sizeof(monitor), "%s.monitor", argv[1]);
  buffer_attr attr = {UINT32_MAX, 1920, 0, 480, 960};
  capture = simple_new(
    NULL, "innkeeper-bench", 2, monitor, "capture", &spec, NULL, &attr, NULL
  );
  pa_simple* playback = simple_new(
    NULL, "innkeeper-bench", 1, argv[1], "playback", &spec, NULL, &attr, NULL
  );
  if (!capture || !playback) return 4;
  uint32_t state = 1;
  for (size_t i = 0; i < 127; i++) {
    state = state * 1664525 + 1013904223;
    marker[i] = state & 0x80000000 ? 16000 : -16000;
  }
  pthread_t thread;
  if (pthread_create(&thread, NULL, record, NULL)) return 5;
  int16_t samples[4800] = {0};
  for (size_t i = 0; i < 127; i++) samples[i] = marker[i];
  uint64_t start = now();
  if (simple_write(playback, samples, sizeof(samples), NULL) < 0) return 6;
  uint64_t latency = simple_get_latency(playback, NULL);
  int64_t first_sample =
    (int64_t)(now() - start) + (int64_t)(latency * 1000) - 100000000;
  pthread_join(thread, NULL);
  printf(
    "{\"first_sample_output_estimate_ns\":%lld,\"queued_output_us\":%llu,"
    "\"roundtrip_upper_bound_ns\":%llu,\"marker_"
    "detected\":%s}\n",
    (long long)first_sample, (unsigned long long)latency,
    (unsigned long long)(detected > start ? detected - start : 0),
    detected > start ? "true" : "false"
  );
  simple_free(capture);
  simple_free(playback);
  dlclose(lib);
  return detected > start && latency != UINT64_MAX ? 0 : 7;
}
