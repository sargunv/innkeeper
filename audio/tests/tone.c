// linuxapk-tone: plays a sine through AAudio (the app-facing path: AAudio ->
// AudioFlinger -> audio HAL -> our tinyalsa -> bridge -> PipeWire) and
// reports output latency the way Oboe does: from AAudioStream_getTimestamp,
// latency = (presentation time of the last written frame) - now.
//
//   linuxapk-tone [seconds=3] [hz=440] [lowlatency=1] [amplitude=0.5]

#include <aaudio/AAudio.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
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
  int low = argc > 3 ? atoi(argv[3]) : 1;
  float amp = argc > 4 ? (float)atof(argv[4]) : 0.5f;

  AAudioStreamBuilder* b;
  if (AAudio_createStreamBuilder(&b) != AAUDIO_OK) return 1;
  AAudioStreamBuilder_setDirection(b, AAUDIO_DIRECTION_OUTPUT);
  AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_FLOAT);
  AAudioStreamBuilder_setChannelCount(b, 2);
  AAudioStreamBuilder_setSampleRate(b, 48000);
  AAudioStreamBuilder_setUsage(b, AAUDIO_USAGE_MEDIA);
  AAudioStreamBuilder_setPerformanceMode(
    b, low ? AAUDIO_PERFORMANCE_MODE_LOW_LATENCY : AAUDIO_PERFORMANCE_MODE_NONE
  );
  AAudioStream* s;
  aaudio_result_t r = AAudioStreamBuilder_openStream(b, &s);
  AAudioStreamBuilder_delete(b);
  if (r != AAUDIO_OK) {
    fprintf(stderr, "openStream: %s\n", AAudio_convertResultToText(r));
    return 1;
  }
  int rate = AAudioStream_getSampleRate(s);
  int burst = AAudioStream_getFramesPerBurst(s);
  printf(
    "stream: %d Hz, burst %d, buffer %d/%d frames, perf mode %d, sharing %d\n",
    rate, burst, AAudioStream_getBufferSizeInFrames(s),
    AAudioStream_getBufferCapacityInFrames(s),
    AAudioStream_getPerformanceMode(s), AAudioStream_getSharingMode(s)
  );
  if (AAudioStream_requestStart(s) != AAUDIO_OK) return 1;

  int chunk = burst > 0 ? burst : 192;
  float* buf = malloc(sizeof(float) * 2 * (size_t)chunk);
  int64_t total = (int64_t)(secs * rate), written = 0;
  double lat[4096];
  int nlat = 0;
  int64_t next_sample = now_ns() + 500000000;  // skip start-up
  while (written < total) {
    for (int i = 0; i < chunk; i++) {
      float v =
        amp * (float)sin(2.0 * M_PI * hz * (double)(written + i) / rate);
      buf[2 * i] = buf[2 * i + 1] = v;
    }
    aaudio_result_t w = AAudioStream_write(s, buf, chunk, 1000000000LL);
    if (w < 0) {
      fprintf(stderr, "write: %s\n", AAudio_convertResultToText(w));
      break;
    }
    written += w;
    int64_t t = now_ns();
    if (t >= next_sample && nlat < 4096) {
      int64_t pos, when;
      if (
        AAudioStream_getTimestamp(s, CLOCK_MONOTONIC, &pos, &when) == AAUDIO_OK
      ) {
        int64_t app = AAudioStream_getFramesWritten(s);
        double ms =
          ((double)when + (double)(app - pos) * 1e9 / rate - (double)t) / 1e6;
        lat[nlat++] = ms;
      }
      next_sample = t + 50000000;
    }
  }
  AAudioStream_requestStop(s);
  printf(
    "wrote %.2f s at %.1f Hz, xruns %d\n", (double)written / rate, hz,
    AAudioStream_getXRunCount(s)
  );
  if (nlat > 0) {
    qsort(lat, (size_t)nlat, sizeof(double), cmp);
    printf(
      "output latency: median %.1f ms, p10 %.1f, p90 %.1f, min %.1f, max %.1f "
      "(%d samples)\n",
      lat[nlat / 2], lat[nlat / 10], lat[nlat * 9 / 10], lat[0], lat[nlat - 1],
      nlat
    );
  } else {
    printf("output latency: no timestamps\n");
  }
  AAudioStream_close(s);
  free(buf);
  return 0;
}
