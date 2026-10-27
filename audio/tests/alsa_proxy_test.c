// alsa_proxy_test: drives an image's own libalsautilsv2 (AOSP alsa_utils,
// what the AIDL default audio HAL's primary module uses) against our
// libtinyalsav2, the way StreamPrimary does for the built-in speaker:
// profile_fill_builtin_device_info -> proxy_prepare_from_default_config ->
// proxy_open -> proxy_write_with_retries, and reports the presentation
// position the HAL would give AudioFlinger.
//
//   LD_LIBRARY_PATH=<dir with the image's APEX libs + our libtinyalsav2.so> \
//     alsa_proxy_test [seconds=3] [hz=440] [buffer_frames=960]
//
// Latency printed = frames written - frames presented (+ time since the
// position's timestamp), i.e. what sits between the HAL and the listener.

#include <alsa_device_profile.h>
#include <alsa_device_proxy.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static int cmp(const void* a, const void* b) {
  double x = *(const double*)a, y = *(const double*)b;
  return x < y ? -1 : x > y;
}

int main(int argc, char** argv) {
  setvbuf(stdout, NULL, _IOLBF, 0);
  double secs = argc > 1 ? atof(argv[1]) : 3.0;
  double hz = argc > 2 ? atof(argv[2]) : 440.0;
  unsigned buffer_frames = argc > 3 ? (unsigned)atoi(argv[3]) : 960;
  const unsigned rate = 48000;

  alsa_device_profile profile;
  profile_init(&profile, PCM_OUT);
  profile.card = 0;
  profile.device = 0;
  struct pcm_config cfg = {
    .channels = 2, .rate = rate, .format = PCM_FORMAT_S16_LE
  };
  if (!profile_fill_builtin_device_info(&profile, &cfg, buffer_frames)) {
    fprintf(stderr, "profile_fill_builtin_device_info failed\n");
    return 1;
  }
  alsa_device_proxy proxy;
  memset(&proxy, 0, sizeof(proxy));
  if (
    proxy_prepare_from_default_config(&proxy, &profile) != 0 ||
    proxy_open(&proxy) != 0
  ) {
    fprintf(stderr, "proxy open failed\n");
    return 1;
  }
  printf(
    "proxy: period %u x %u frames, proxy_get_latency %u ms\n",
    proxy_get_period_size(&proxy), proxy.alsa_config.period_count,
    proxy_get_latency(&proxy)
  );

  unsigned chunk =
    buffer_frames;  // StreamAlsa transfers a HAL buffer at a time
  int16_t* buf = malloc(chunk * 4);
  uint64_t total = (uint64_t)(secs * rate), done = 0;
  double lat[4096];
  int nlat = 0;
  int64_t t0 = now_ns(), next = t0 + 500000000;
  while (done < total) {
    for (unsigned i = 0; i < chunk; i++) {
      int16_t v =
        (int16_t)(0.5 * 32767 * sin(2 * M_PI * hz * (double)(done + i) / rate));
      buf[2 * i] = buf[2 * i + 1] = v;
    }
    if (proxy_write_with_retries(&proxy, buf, chunk * 4, 3) != 0) {
      fprintf(stderr, "proxy_write failed\n");
      break;
    }
    done += chunk;
    int64_t t = now_ns();
    uint64_t frames;
    struct timespec ts;
    if (
      t >= next && nlat < 4096 &&
      proxy_get_presentation_position(&proxy, &frames, &ts) == 0
    ) {
      int64_t tsn = (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
      lat[nlat++] = ((double)(proxy.transferred - frames) * 1e3 / rate) -
                    (double)(t - tsn) / 1e6;
      next = t + 50000000;
    }
  }
  double el = (double)(now_ns() - t0) / 1e9;
  printf("wrote %.2f s of %.0f Hz in %.2f s\n", (double)done / rate, hz, el);
  if (nlat) {
    qsort(lat, (size_t)nlat, sizeof(double), cmp);
    printf(
      "HAL->listener latency: median %.1f ms, p10 %.1f, p90 %.1f (%d "
      "samples)\n",
      lat[nlat / 2], lat[nlat / 10], lat[nlat * 9 / 10], nlat
    );
  }
  proxy_close(&proxy);
  return 0;
}
