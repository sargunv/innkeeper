// The host has no Android bridge socket: capture must remain a paced device.
#include <assert.h>
#include <stdint.h>
#include <string.h>
#include <time.h>
#include <tinyalsa/pcm.h>

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

int main(void) {
  struct pcm_config cfg = {
    .channels = 1,
    .rate = 48000,
    .format = PCM_FORMAT_S16_LE,
    .period_size = 480,
    .period_count = 2
  };
  struct pcm* pcm = pcm_open(0, 0, PCM_IN, &cfg);
  assert(pcm_is_ready(pcm));
  int16_t samples[480];
  int64_t start = now_ns();
  for (int i = 0; i < 3; ++i) {
    memset(samples, 0x55, sizeof(samples));
    assert(pcm_readi(pcm, samples, 480) == 480);
    for (int j = 0; j < 480; ++j) assert(samples[j] == 0);
  }
  assert(now_ns() - start >= 30000000);
  assert(pcm_stop(pcm) == 0);
  start = now_ns();
  assert(pcm_readi(pcm, samples, 480) == 480);
  assert(now_ns() - start >= 10000000);
  assert(pcm_close(pcm) == 0);
  return 0;
}
