// The host has no Android bridge socket, so capture must stay a paced
// device.
#include <assert.h>
#include <stdint.h>
#include <string.h>
#include <time.h>

#include "innkeeper/pcm.h"

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

int main(void) {
  struct bridge_pcm* pcm = bridge_pcm_open(true, 48000, 1, 480, 2, "Test");
  assert(pcm);
  assert(!bridge_pcm_open(true, 48000, 3, 480, 2, "Test"));
  int16_t samples[480];
  int64_t start = now_ns();
  for (int i = 0; i < 3; ++i) {
    memset(samples, 0x55, sizeof(samples));
    assert(bridge_pcm_read(pcm, samples, sizeof(samples)) == 0);
    for (int j = 0; j < 480; ++j) assert(samples[j] == 0);
  }
  assert(now_ns() - start >= 30000000);
  bridge_pcm_stop(pcm);
  start = now_ns();
  assert(bridge_pcm_read(pcm, samples, sizeof(samples)) == 0);
  assert(now_ns() - start >= 10000000);
  bridge_pcm_close(pcm);
  return 0;
}
