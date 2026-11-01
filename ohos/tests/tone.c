// SPDX-License-Identifier: AGPL-3.0-or-later
// A test tool for the e2e audio checks, built against OH's public OHAudio
// API (libohaudio.so). It never ships in the layer. The test copies it into
// the profile and runs it over exec.
//
//   tone play MS    plays a 440 Hz tone at 48 kHz for MS milliseconds
//   tone record MS  writes MS milliseconds of the microphone to stdout as
//                   48 kHz stereo s16le
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

// From OH's native_audiostream_base.h, native_audiostreambuilder.h,
// native_audiorenderer.h and native_audiocapturer.h.
typedef struct OH_AudioStreamBuilderStruct OH_AudioStreamBuilder;
typedef struct OH_AudioRendererStruct OH_AudioRenderer;
typedef struct OH_AudioCapturerStruct OH_AudioCapturer;
enum { AUDIOSTREAM_TYPE_RENDERER = 1, AUDIOSTREAM_TYPE_CAPTURER = 2 };
enum { AUDIOSTREAM_SAMPLE_S16LE = 1 };
enum { AUDIOSTREAM_USAGE_MUSIC = 1 };
enum { AUDIOSTREAM_SOURCE_TYPE_MIC = 0 };
enum { AUDIO_DATA_CALLBACK_RESULT_VALID = 1 };
int OH_AudioStreamBuilder_Create(OH_AudioStreamBuilder** b, int type);
int OH_AudioStreamBuilder_Destroy(OH_AudioStreamBuilder* b);
int OH_AudioStreamBuilder_SetSamplingRate(OH_AudioStreamBuilder* b, int rate);
int OH_AudioStreamBuilder_SetChannelCount(OH_AudioStreamBuilder* b, int n);
int OH_AudioStreamBuilder_SetSampleFormat(OH_AudioStreamBuilder* b, int fmt);
int OH_AudioStreamBuilder_SetRendererInfo(OH_AudioStreamBuilder* b, int usage);
int OH_AudioStreamBuilder_SetCapturerInfo(OH_AudioStreamBuilder* b, int src);
int OH_AudioStreamBuilder_SetRendererWriteDataCallback(
  OH_AudioStreamBuilder* b,
  int (*cb)(OH_AudioRenderer*, void* user, void* data, int32_t bytes),
  void* user
);
int OH_AudioStreamBuilder_SetCapturerReadDataCallback(
  OH_AudioStreamBuilder* b,
  void (*cb)(OH_AudioCapturer*, void* user, void* data, int32_t bytes),
  void* user
);
int OH_AudioStreamBuilder_GenerateRenderer(
  OH_AudioStreamBuilder* b, OH_AudioRenderer** r
);
int OH_AudioStreamBuilder_GenerateCapturer(
  OH_AudioStreamBuilder* b, OH_AudioCapturer** c
);
int OH_AudioRenderer_Start(OH_AudioRenderer* r);
int OH_AudioRenderer_Stop(OH_AudioRenderer* r);
int OH_AudioRenderer_Release(OH_AudioRenderer* r);
int OH_AudioCapturer_Start(OH_AudioCapturer* c);
int OH_AudioCapturer_Stop(OH_AudioCapturer* c);
int OH_AudioCapturer_Release(OH_AudioCapturer* c);

#define RATE 48000
#define CHANNELS 2

static uint64_t g_phase;

static int write_tone(OH_AudioRenderer* r, void* user, void* data, int32_t n) {
  (void)r;
  (void)user;
  int16_t* s = data;
  for (int32_t i = 0; i < n / (2 * CHANNELS); i++, g_phase++) {
    int16_t v = (int16_t)(8000 * sin(2 * M_PI * 440 * g_phase / RATE));
    for (int c = 0; c < CHANNELS; c++) s[i * CHANNELS + c] = v;
  }
  return AUDIO_DATA_CALLBACK_RESULT_VALID;
}

static void read_mic(OH_AudioCapturer* c, void* user, void* data, int32_t n) {
  (void)c;
  (void)user;
  fwrite(data, 1, (size_t)n, stdout);
}

static int fail(const char* what, int r) {
  fprintf(stderr, "tone: %s failed (%d)\n", what, r);
  return 1;
}

static void sleep_ms(long ms) {
  struct timespec ts = {ms / 1000, ms % 1000 * 1000000};
  nanosleep(&ts, NULL);
}

int main(int argc, char** argv) {
  if (argc != 3 || (strcmp(argv[1], "play") && strcmp(argv[1], "record"))) {
    fprintf(stderr, "usage: tone play|record MS\n");
    return 2;
  }
  bool play = !strcmp(argv[1], "play");
  long ms = atol(argv[2]);
  OH_AudioStreamBuilder* b;
  int r = OH_AudioStreamBuilder_Create(
    &b, play ? AUDIOSTREAM_TYPE_RENDERER : AUDIOSTREAM_TYPE_CAPTURER
  );
  if (r) return fail("create", r);
  OH_AudioStreamBuilder_SetSamplingRate(b, RATE);
  OH_AudioStreamBuilder_SetChannelCount(b, CHANNELS);
  OH_AudioStreamBuilder_SetSampleFormat(b, AUDIOSTREAM_SAMPLE_S16LE);
  if (play) {
    OH_AudioRenderer* ren;
    OH_AudioStreamBuilder_SetRendererInfo(b, AUDIOSTREAM_USAGE_MUSIC);
    OH_AudioStreamBuilder_SetRendererWriteDataCallback(b, write_tone, NULL);
    if ((r = OH_AudioStreamBuilder_GenerateRenderer(b, &ren)))
      return fail("renderer", r);
    if ((r = OH_AudioRenderer_Start(ren))) return fail("start", r);
    fprintf(stderr, "tone: playing\n");
    sleep_ms(ms);
    OH_AudioRenderer_Stop(ren);
    OH_AudioRenderer_Release(ren);
  } else {
    OH_AudioCapturer* cap;
    OH_AudioStreamBuilder_SetCapturerInfo(b, AUDIOSTREAM_SOURCE_TYPE_MIC);
    OH_AudioStreamBuilder_SetCapturerReadDataCallback(b, read_mic, NULL);
    if ((r = OH_AudioStreamBuilder_GenerateCapturer(b, &cap)))
      return fail("capturer", r);
    if ((r = OH_AudioCapturer_Start(cap))) return fail("start", r);
    sleep_ms(ms);
    OH_AudioCapturer_Stop(cap);
    OH_AudioCapturer_Release(cap);
    fflush(stdout);
  }
  OH_AudioStreamBuilder_Destroy(b);
  return 0;
}
