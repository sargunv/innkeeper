// SPDX-License-Identifier: AGPL-3.0-or-later
// OpenHarmony's primary audio VDI (vdi.h) over the bridge's audio streams.
// The image's audio_host loads OH's IDL-to-VDI adapter, which loads this
// library as /vendor/lib64/libaudio_primary_impl.z.so. Each render and
// capture is a blocking PCM device from guest/audio-client.
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "innkeeper/pcm.h"
#include "vdi.h"

#ifdef INNKEEPER_HILOG
#include <stdarg.h>
#include <stdio.h>
int HiLogPrint(
  int type, int level, unsigned int domain, const char* tag, const char* fmt,
  ...
);
// hilog hides unmarked arguments, so the line is formatted here.
__attribute__((format(printf, 1, 2))) static void logi(const char* fmt, ...) {
  char line[256];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof(line), fmt, ap);
  va_end(ap);
  HiLogPrint(3, 4, 0xD0025FF, "innkeeper-audio", "%{public}s", line);
}
#else
#define logi(...) ((void)0)
#endif

#define EXPORT __attribute__((visibility("default")))

#define ADAPTER_NAME "primary"
#define OUT_PORT 0
#define IN_PORT 1
#define STREAM_NAME "OpenHarmony"
// Playback bursts are 5 ms, and the ring holds two of them, as in Android's
// HAL. The bridge asks PipeWire for another 20 ms downstream
// (host/bridge/src/audio/pulse.rs, TARGET_LATENCY_US).
#define OUT_PERIOD_MS 5
#define OUT_PERIODS 2
#define DOWNSTREAM_MS 20
// Capture can't hold back its source, so its ring absorbs 80 ms of
// scheduling jitter.
#define IN_PERIOD_MS 10
#define IN_PERIODS 8

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static void stamp_now(struct AudioTimeStampVdi* t) {
  int64_t ns = now_ns();
  t->tvSec = ns / 1000000000;
  t->tvNSec = ns % 1000000000;
}

// The PCM part of the attributes the VDI accepts.
struct pcm_cfg {
  uint32_t rate, channels;
};

// OH asks for 16-bit interleaved PCM, at the policy's rate, in one or two
// channels. pcm.h supports nothing else.
static int parse_attrs(
  const struct AudioSampleAttributesVdi* a, struct pcm_cfg* c
) {
  if (
    !a || a->format != AUDIO_VDI_FORMAT_TYPE_PCM_16_BIT || a->isBigEndian ||
    a->channelCount < 1 || a->channelCount > 2 || a->sampleRate < 8000 ||
    a->sampleRate > 192000
  )
    return HDF_ERR_NOT_SUPPORT;
  c->rate = a->sampleRate;
  c->channels = a->channelCount;
  return HDF_SUCCESS;
}

static void fill_attrs(
  struct AudioSampleAttributesVdi* a, const struct pcm_cfg* c, bool input
) {
  a->type = 0;  // AUDIO_IN_MEDIA
  a->interleaved = true;
  a->format = AUDIO_VDI_FORMAT_TYPE_PCM_16_BIT;
  a->sampleRate = c->rate;
  a->channelCount = c->channels;
  a->channelLayout =
    c->channels == 1 ? AUDIO_VDI_CHANNEL_MONO : AUDIO_VDI_CHANNEL_STEREO;
  a->period = c->rate * (input ? IN_PERIOD_MS : OUT_PERIOD_MS) / 1000;
  a->frameSize = c->channels * 2;
  a->isBigEndian = false;
  a->isSignedData = true;
}

static uint32_t frame_bytes(const struct pcm_cfg* c) { return c->channels * 2; }

static int32_t no_scene(const struct AudioSceneDescriptorVdi* s, bool* ok) {
  if (!s || !ok) return HDF_ERR_INVALID_PARAM;
  *ok = true;
  return HDF_SUCCESS;
}

// ---- render ----

struct render {
  struct IAudioRenderVdi vt;  // first, and per render: the adapter edits it
  struct pcm_cfg cfg;
  struct bridge_pcm* pcm;
  uint64_t written;  // frames accepted since creation (atomic access)
  bool mute;
  float volume;
};

static struct render* R(struct IAudioRenderVdi* v) { return (struct render*)v; }

static uint32_t out_period(const struct render* r) {
  return r->cfg.rate * OUT_PERIOD_MS / 1000;
}

static int32_t r_latency(struct IAudioRenderVdi* v, uint32_t* ms) {
  if (!ms) return HDF_ERR_INVALID_PARAM;
  *ms = OUT_PERIOD_MS * OUT_PERIODS + DOWNSTREAM_MS;
  (void)v;
  return HDF_SUCCESS;
}

static int32_t r_frame(
  struct IAudioRenderVdi* v, const int8_t* frame, uint32_t len, uint64_t* reply
) {
  struct render* r = R(v);
  if (!frame || !reply) return HDF_ERR_INVALID_PARAM;
  uint32_t fb = frame_bytes(&r->cfg);
  len -= len % fb;
  int ret;
  if (__atomic_load_n(&r->mute, __ATOMIC_RELAXED)) {
    int8_t* zero = calloc(1, len ? len : 1);
    if (!zero) return HDF_FAILURE;
    ret = bridge_pcm_write(r->pcm, zero, len);
    free(zero);
  } else {
    ret = bridge_pcm_write(r->pcm, frame, len);
  }
  if (ret != 0) return HDF_FAILURE;
  __atomic_fetch_add(&r->written, len / fb, __ATOMIC_RELEASE);
  *reply = len;
  return HDF_SUCCESS;
}

// Frames that have reached the listener, as of the returned time.
static int32_t r_position(
  struct IAudioRenderVdi* v, uint64_t* frames, struct AudioTimeStampVdi* t
) {
  struct render* r = R(v);
  if (!frames || !t) return HDF_ERR_INVALID_PARAM;
  uint64_t w = __atomic_load_n(&r->written, __ATOMIC_ACQUIRE);
  long delay = bridge_pcm_delay(r->pcm);
  *frames = (uint64_t)delay < w ? w - (uint64_t)delay : 0;
  stamp_now(t);
  return HDF_SUCCESS;
}

static int32_t r_supports_drain(struct IAudioRenderVdi* v, bool* support) {
  (void)v;
  if (!support) return HDF_ERR_INVALID_PARAM;
  *support = false;
  return HDF_SUCCESS;
}

static int32_t r_check_scene(
  struct IAudioRenderVdi* v, const struct AudioSceneDescriptorVdi* s, bool* ok
) {
  (void)v;
  return no_scene(s, ok);
}

static int32_t r_select_scene(
  struct IAudioRenderVdi* v, const struct AudioSceneDescriptorVdi* s
) {
  (void)v;
  return s ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

static int32_t r_set_mute(struct IAudioRenderVdi* v, bool mute) {
  __atomic_store_n(&R(v)->mute, mute, __ATOMIC_RELAXED);
  return HDF_SUCCESS;
}

static int32_t r_get_mute(struct IAudioRenderVdi* v, bool* mute) {
  if (!mute) return HDF_ERR_INVALID_PARAM;
  *mute = __atomic_load_n(&R(v)->mute, __ATOMIC_RELAXED);
  return HDF_SUCCESS;
}

// The desktop mixer is the volume control. OH's system volume stays at its
// maximum (const.multimedia.audio.fixedvolume), and a volume set here is
// reported back but not applied, so the two don't stack.
static int32_t r_set_volume(struct IAudioRenderVdi* v, float volume) {
  if (volume < 0 || volume > 1) return HDF_ERR_INVALID_PARAM;
  R(v)->volume = volume;
  return HDF_SUCCESS;
}

static int32_t r_set_volume_ramp(
  struct IAudioRenderVdi* v, float volume, uint32_t ms
) {
  (void)ms;
  return r_set_volume(v, volume);
}

static int32_t r_get_volume(struct IAudioRenderVdi* v, float* volume) {
  if (!volume) return HDF_ERR_INVALID_PARAM;
  *volume = R(v)->volume;
  return HDF_SUCCESS;
}

static int32_t r_gain_threshold(
  struct IAudioRenderVdi* v, float* min, float* max
) {
  (void)v;
  if (!min || !max) return HDF_ERR_INVALID_PARAM;
  *min = *max = 0;
  return HDF_SUCCESS;
}

static int32_t r_get_gain(struct IAudioRenderVdi* v, float* gain) {
  (void)v;
  if (!gain) return HDF_ERR_INVALID_PARAM;
  *gain = 0;
  return HDF_SUCCESS;
}

static int32_t r_set_gain(struct IAudioRenderVdi* v, float gain) {
  (void)v;
  (void)gain;
  return HDF_SUCCESS;
}

static int32_t r_frame_size(struct IAudioRenderVdi* v, uint64_t* size) {
  if (!size) return HDF_ERR_INVALID_PARAM;
  *size = frame_bytes(&R(v)->cfg);
  return HDF_SUCCESS;
}

static int32_t r_frame_count(struct IAudioRenderVdi* v, uint64_t* count) {
  if (!count) return HDF_ERR_INVALID_PARAM;
  *count = __atomic_load_n(&R(v)->written, __ATOMIC_ACQUIRE);
  return HDF_SUCCESS;
}

static int32_t r_set_attrs(
  struct IAudioRenderVdi* v, const struct AudioSampleAttributesVdi* a
) {
  struct pcm_cfg c;
  int32_t ret = parse_attrs(a, &c);
  if (ret != HDF_SUCCESS) return ret;
  // The ring's format is fixed when the render is created.
  struct pcm_cfg* cur = &R(v)->cfg;
  return c.rate == cur->rate && c.channels == cur->channels
           ? HDF_SUCCESS
           : HDF_ERR_NOT_SUPPORT;
}

static int32_t r_get_attrs(
  struct IAudioRenderVdi* v, struct AudioSampleAttributesVdi* a
) {
  if (!a) return HDF_ERR_INVALID_PARAM;
  fill_attrs(a, &R(v)->cfg, false);
  return HDF_SUCCESS;
}

static int32_t r_channel_id(struct IAudioRenderVdi* v, uint32_t* id) {
  if (!id) return HDF_ERR_INVALID_PARAM;
  *id = R(v)->cfg.channels;
  return HDF_SUCCESS;
}

static int32_t r_set_extra(struct IAudioRenderVdi* v, const char* kv) {
  (void)v;
  if (!kv) return HDF_ERR_INVALID_PARAM;
  logi("render parameter %s", kv);
  return HDF_SUCCESS;
}

static int32_t r_get_extra(struct IAudioRenderVdi* v, char* kv, uint32_t len) {
  (void)v;
  if (!kv || !len) return HDF_ERR_INVALID_PARAM;
  kv[0] = 0;
  return HDF_SUCCESS;
}

static int32_t r_buffer_size(struct IAudioRenderVdi* v, uint64_t* size) {
  struct render* r = R(v);
  if (!size) return HDF_ERR_INVALID_PARAM;
  *size = (uint64_t)out_period(r) * OUT_PERIODS * frame_bytes(&r->cfg);
  return HDF_SUCCESS;
}

static int32_t r_start(struct IAudioRenderVdi* v) {
  (void)v;
  return HDF_SUCCESS;
}

// The next write reopens the bridge stream. Positions keep counting.
static int32_t r_stop(struct IAudioRenderVdi* v) {
  bridge_pcm_stop(R(v)->pcm);
  return HDF_SUCCESS;
}

static int32_t r_dump(struct IAudioRenderVdi* v, int32_t range, int32_t fd) {
  (void)v;
  (void)range;
  (void)fd;
  return HDF_SUCCESS;
}

static int32_t r_supports_pause(
  struct IAudioRenderVdi* v, bool* pause, bool* resume
) {
  (void)v;
  if (!pause || !resume) return HDF_ERR_INVALID_PARAM;
  *pause = *resume = true;
  return HDF_SUCCESS;
}

static int32_t r_set_buffer_size(struct IAudioRenderVdi* v, uint32_t size) {
  (void)v;
  (void)size;
  return HDF_SUCCESS;
}

static const struct IAudioRenderVdi render_vt = {
  .GetLatency = r_latency,
  .RenderFrame = r_frame,
  .GetRenderPosition = r_position,
  .IsSupportsDrain = r_supports_drain,
  .CheckSceneCapability = r_check_scene,
  .SelectScene = r_select_scene,
  .SetMute = r_set_mute,
  .GetMute = r_get_mute,
  .SetVolume = r_set_volume,
  .SetVolumeWithRamp = r_set_volume_ramp,
  .GetVolume = r_get_volume,
  .GetGainThreshold = r_gain_threshold,
  .GetGain = r_get_gain,
  .SetGain = r_set_gain,
  .GetFrameSize = r_frame_size,
  .GetFrameCount = r_frame_count,
  .SetSampleAttributes = r_set_attrs,
  .GetSampleAttributes = r_get_attrs,
  .GetCurrentChannelId = r_channel_id,
  .SetExtraParams = r_set_extra,
  .GetExtraParams = r_get_extra,
  .GetFrameBufferSize = r_buffer_size,
  .Start = r_start,
  .Stop = r_stop,
  .Pause = r_stop,
  .Resume = r_start,
  .Flush = r_stop,
  .TurnStandbyMode = r_stop,
  .AudioDevDump = r_dump,
  .IsSupportsPauseAndResume = r_supports_pause,
  .SetBufferSize = r_set_buffer_size,
};

// ---- capture ----

struct capture {
  struct IAudioCaptureVdi vt;  // first, and per capture
  struct pcm_cfg cfg;
  struct bridge_pcm* pcm;
  uint64_t read;  // frames returned since creation (atomic access)
};

static struct capture* C(struct IAudioCaptureVdi* v) {
  return (struct capture*)v;
}

static bool g_mic_mute;

static bool capture_muted(void) {
  return __atomic_load_n(&g_mic_mute, __ATOMIC_RELAXED);
}

static int32_t c_frame(
  struct IAudioCaptureVdi* v, int8_t* frame, uint32_t* len, uint64_t* reply
) {
  struct capture* c = C(v);
  if (!frame || !len || !reply) return HDF_ERR_INVALID_PARAM;
  uint32_t fb = frame_bytes(&c->cfg);
  uint32_t n = *len - *len % fb;
  if (bridge_pcm_read(c->pcm, frame, n) != 0) return HDF_FAILURE;
  if (capture_muted()) memset(frame, 0, n);
  __atomic_fetch_add(&c->read, n / fb, __ATOMIC_RELEASE);
  *reply = n;
  return HDF_SUCCESS;
}

static int32_t c_position(
  struct IAudioCaptureVdi* v, uint64_t* frames, struct AudioTimeStampVdi* t
) {
  if (!frames || !t) return HDF_ERR_INVALID_PARAM;
  *frames = __atomic_load_n(&C(v)->read, __ATOMIC_ACQUIRE);
  stamp_now(t);
  return HDF_SUCCESS;
}

static int32_t c_check_scene(
  struct IAudioCaptureVdi* v, const struct AudioSceneDescriptorVdi* s, bool* ok
) {
  (void)v;
  return no_scene(s, ok);
}

static int32_t c_select_scene(
  struct IAudioCaptureVdi* v, const struct AudioSceneDescriptorVdi* s
) {
  (void)v;
  return s ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

static int32_t c_set_mute(struct IAudioCaptureVdi* v, bool mute) {
  (void)v;
  __atomic_store_n(&g_mic_mute, mute, __ATOMIC_RELAXED);
  return HDF_SUCCESS;
}

static int32_t c_get_mute(struct IAudioCaptureVdi* v, bool* mute) {
  (void)v;
  if (!mute) return HDF_ERR_INVALID_PARAM;
  *mute = capture_muted();
  return HDF_SUCCESS;
}

static int32_t c_set_volume(struct IAudioCaptureVdi* v, float volume) {
  (void)v;
  return volume < 0 || volume > 1 ? HDF_ERR_INVALID_PARAM : HDF_SUCCESS;
}

static int32_t c_get_volume(struct IAudioCaptureVdi* v, float* volume) {
  (void)v;
  if (!volume) return HDF_ERR_INVALID_PARAM;
  *volume = 1;
  return HDF_SUCCESS;
}

static int32_t c_gain_threshold(
  struct IAudioCaptureVdi* v, float* min, float* max
) {
  (void)v;
  if (!min || !max) return HDF_ERR_INVALID_PARAM;
  *min = *max = 0;
  return HDF_SUCCESS;
}

static int32_t c_get_gain(struct IAudioCaptureVdi* v, float* gain) {
  (void)v;
  if (!gain) return HDF_ERR_INVALID_PARAM;
  *gain = 0;
  return HDF_SUCCESS;
}

static int32_t c_set_gain(struct IAudioCaptureVdi* v, float gain) {
  (void)v;
  (void)gain;
  return HDF_SUCCESS;
}

static int32_t c_frame_size(struct IAudioCaptureVdi* v, uint64_t* size) {
  if (!size) return HDF_ERR_INVALID_PARAM;
  *size = frame_bytes(&C(v)->cfg);
  return HDF_SUCCESS;
}

static int32_t c_frame_count(struct IAudioCaptureVdi* v, uint64_t* count) {
  if (!count) return HDF_ERR_INVALID_PARAM;
  *count = __atomic_load_n(&C(v)->read, __ATOMIC_ACQUIRE);
  return HDF_SUCCESS;
}

static int32_t c_set_attrs(
  struct IAudioCaptureVdi* v, const struct AudioSampleAttributesVdi* a
) {
  struct pcm_cfg c;
  int32_t ret = parse_attrs(a, &c);
  if (ret != HDF_SUCCESS) return ret;
  struct pcm_cfg* cur = &C(v)->cfg;
  return c.rate == cur->rate && c.channels == cur->channels
           ? HDF_SUCCESS
           : HDF_ERR_NOT_SUPPORT;
}

static int32_t c_get_attrs(
  struct IAudioCaptureVdi* v, struct AudioSampleAttributesVdi* a
) {
  if (!a) return HDF_ERR_INVALID_PARAM;
  fill_attrs(a, &C(v)->cfg, true);
  return HDF_SUCCESS;
}

static int32_t c_channel_id(struct IAudioCaptureVdi* v, uint32_t* id) {
  if (!id) return HDF_ERR_INVALID_PARAM;
  *id = C(v)->cfg.channels;
  return HDF_SUCCESS;
}

static int32_t c_set_extra(struct IAudioCaptureVdi* v, const char* kv) {
  (void)v;
  return kv ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

static int32_t c_get_extra(struct IAudioCaptureVdi* v, char* kv, uint32_t len) {
  (void)v;
  if (!kv || !len) return HDF_ERR_INVALID_PARAM;
  kv[0] = 0;
  return HDF_SUCCESS;
}

static int32_t c_start(struct IAudioCaptureVdi* v) {
  (void)v;
  return HDF_SUCCESS;
}

// Standby closes the host record stream, and the next read opens a fresh
// ring.
static int32_t c_stop(struct IAudioCaptureVdi* v) {
  bridge_pcm_stop(C(v)->pcm);
  return HDF_SUCCESS;
}

static int32_t c_dump(struct IAudioCaptureVdi* v, int32_t range, int32_t fd) {
  (void)v;
  (void)range;
  (void)fd;
  return HDF_SUCCESS;
}

static const struct IAudioCaptureVdi capture_vt = {
  .CaptureFrame = c_frame,
  .GetCapturePosition = c_position,
  .CheckSceneCapability = c_check_scene,
  .SelectScene = c_select_scene,
  .SetMute = c_set_mute,
  .GetMute = c_get_mute,
  .SetVolume = c_set_volume,
  .GetVolume = c_get_volume,
  .GetGainThreshold = c_gain_threshold,
  .GetGain = c_get_gain,
  .SetGain = c_set_gain,
  .GetFrameSize = c_frame_size,
  .GetFrameCount = c_frame_count,
  .SetSampleAttributes = c_set_attrs,
  .GetSampleAttributes = c_get_attrs,
  .GetCurrentChannelId = c_channel_id,
  .SetExtraParams = c_set_extra,
  .GetExtraParams = c_get_extra,
  .Start = c_start,
  .Stop = c_stop,
  .Pause = c_stop,
  .Resume = c_start,
  .Flush = c_stop,
  .TurnStandbyMode = c_stop,
  .AudioDevDump = c_dump,
};

// ---- adapter ----

static int32_t a_init_ports(struct IAudioAdapterVdi* a) {
  (void)a;
  return HDF_SUCCESS;
}

static int32_t a_create_render(
  struct IAudioAdapterVdi* a, const struct AudioDeviceDescriptorVdi* desc,
  const struct AudioSampleAttributesVdi* attrs, struct IAudioRenderVdi** out
) {
  (void)a;
  struct pcm_cfg cfg;
  if (!desc || !out) return HDF_ERR_INVALID_PARAM;
  if (parse_attrs(attrs, &cfg) != HDF_SUCCESS) {
    logi(
      "render: unsupported format %d, %u Hz, %u channels",
      attrs ? attrs->format : -1, attrs ? attrs->sampleRate : 0,
      attrs ? attrs->channelCount : 0
    );
    return HDF_ERR_NOT_SUPPORT;
  }
  struct render* r = calloc(1, sizeof(*r));
  if (!r) return HDF_FAILURE;
  r->vt = render_vt;
  r->cfg = cfg;
  r->volume = 1;
  r->pcm = bridge_pcm_open(
    false, cfg.rate, cfg.channels, out_period(r), OUT_PERIODS, STREAM_NAME
  );
  if (!r->pcm) {
    free(r);
    return HDF_FAILURE;
  }
  logi(
    "render: %u Hz, %u channels, pin 0x%x", cfg.rate, cfg.channels, desc->pins
  );
  *out = &r->vt;
  return HDF_SUCCESS;
}

static int32_t a_destroy_render(
  struct IAudioAdapterVdi* a, struct IAudioRenderVdi* v
) {
  (void)a;
  if (!v) return HDF_ERR_INVALID_PARAM;
  bridge_pcm_close(R(v)->pcm);
  free(v);
  return HDF_SUCCESS;
}

static int32_t a_create_capture(
  struct IAudioAdapterVdi* a, const struct AudioDeviceDescriptorVdi* desc,
  const struct AudioSampleAttributesVdi* attrs, struct IAudioCaptureVdi** out
) {
  (void)a;
  struct pcm_cfg cfg;
  if (!desc || !out) return HDF_ERR_INVALID_PARAM;
  if (parse_attrs(attrs, &cfg) != HDF_SUCCESS) return HDF_ERR_NOT_SUPPORT;
  struct capture* c = calloc(1, sizeof(*c));
  if (!c) return HDF_FAILURE;
  c->vt = capture_vt;
  c->cfg = cfg;
  c->pcm = bridge_pcm_open(
    true, cfg.rate, cfg.channels, cfg.rate * IN_PERIOD_MS / 1000, IN_PERIODS,
    STREAM_NAME
  );
  if (!c->pcm) {
    free(c);
    return HDF_FAILURE;
  }
  logi("capture: %u Hz, %u channels", cfg.rate, cfg.channels);
  *out = &c->vt;
  return HDF_SUCCESS;
}

static int32_t a_destroy_capture(
  struct IAudioAdapterVdi* a, struct IAudioCaptureVdi* v
) {
  (void)a;
  if (!v) return HDF_ERR_INVALID_PARAM;
  bridge_pcm_close(C(v)->pcm);
  free(v);
  return HDF_SUCCESS;
}

// The adapter copies these arrays.
static int32_t g_formats[] = {AUDIO_VDI_FORMAT_TYPE_PCM_16_BIT};
static int32_t g_sample_formats[] = {AUDIO_VDI_SAMPLE_FORMAT_S16};
static struct AudioSubPortCapabilityVdi g_sub_out = {
  OUT_PORT, "AOP", AUDIO_VDI_PORT_PASSTHROUGH_LPCM
};
static struct AudioSubPortCapabilityVdi g_sub_in = {
  IN_PORT, "AIP", AUDIO_VDI_PORT_PASSTHROUGH_LPCM
};

static int32_t a_port_capability(
  struct IAudioAdapterVdi* a, const struct AudioPortVdi* port,
  struct AudioPortCapabilityVdi* cap
) {
  (void)a;
  if (!port || !cap) return HDF_ERR_INVALID_PARAM;
  if (port->portId != OUT_PORT && port->portId != IN_PORT)
    return HDF_ERR_NOT_SUPPORT;
  memset(cap, 0, sizeof(*cap));
  cap->deviceType = port->dir;
  cap->deviceId = port->portId;
  cap->formatNum = 1;
  cap->formats = g_formats;
  cap->formatsLen = 1;
  cap->sampleRateMasks =
    AUDIO_VDI_SAMPLE_RATE_MASK_44100 | AUDIO_VDI_SAMPLE_RATE_MASK_48000;
  cap->channelMasks = AUDIO_VDI_CHANNEL_STEREO;
  cap->channelCount = 2;
  cap->subPorts = port->portId == OUT_PORT ? &g_sub_out : &g_sub_in;
  cap->subPortsLen = 1;
  cap->supportSampleFormats = g_sample_formats;
  cap->supportSampleFormatsLen = 1;
  return HDF_SUCCESS;
}

static int32_t a_set_passthrough(
  struct IAudioAdapterVdi* a, const struct AudioPortVdi* port, int32_t mode
) {
  (void)a;
  if (!port) return HDF_ERR_INVALID_PARAM;
  return mode == AUDIO_VDI_PORT_PASSTHROUGH_LPCM ? HDF_SUCCESS
                                                 : HDF_ERR_NOT_SUPPORT;
}

static int32_t a_get_passthrough(
  struct IAudioAdapterVdi* a, const struct AudioPortVdi* port, int32_t* mode
) {
  (void)a;
  if (!port || !mode) return HDF_ERR_INVALID_PARAM;
  *mode = AUDIO_VDI_PORT_PASSTHROUGH_LPCM;
  return HDF_SUCCESS;
}

static int32_t a_device_status(
  struct IAudioAdapterVdi* a, struct AudioDeviceStatusVdi* s
) {
  (void)a;
  if (!s) return HDF_ERR_INVALID_PARAM;
  s->pnpStatus = 0;
  return HDF_SUCCESS;
}

// The session has one output and one input, so every route goes to them.
static int32_t a_update_route(
  struct IAudioAdapterVdi* a, const struct AudioRouteVdi* route, int32_t* h
) {
  (void)a;
  if (!route || !h) return HDF_ERR_INVALID_PARAM;
  *h = 1;
  return HDF_SUCCESS;
}

static int32_t a_release_route(struct IAudioAdapterVdi* a, int32_t h) {
  (void)a;
  (void)h;
  return HDF_SUCCESS;
}

static int32_t a_set_mic_mute(struct IAudioAdapterVdi* a, bool mute) {
  (void)a;
  __atomic_store_n(&g_mic_mute, mute, __ATOMIC_RELAXED);
  return HDF_SUCCESS;
}

static int32_t a_get_mic_mute(struct IAudioAdapterVdi* a, bool* mute) {
  (void)a;
  if (!mute) return HDF_ERR_INVALID_PARAM;
  *mute = capture_muted();
  return HDF_SUCCESS;
}

static int32_t a_voice_volume(struct IAudioAdapterVdi* a, float volume) {
  (void)a;
  (void)volume;
  return HDF_SUCCESS;
}

static int32_t a_set_extra(
  struct IAudioAdapterVdi* a, int32_t key, const char* cond, const char* value
) {
  (void)a;
  (void)key;
  (void)cond;
  (void)value;
  return HDF_SUCCESS;
}

static int32_t a_get_extra(
  struct IAudioAdapterVdi* a, int32_t key, const char* cond, char* value,
  uint32_t len
) {
  (void)a;
  (void)key;
  (void)cond;
  if (value && len) value[0] = 0;
  return HDF_ERR_NOT_SUPPORT;
}

static struct IAudioAdapterVdi g_adapter = {
  .InitAllPorts = a_init_ports,
  .CreateRender = a_create_render,
  .DestroyRender = a_destroy_render,
  .CreateCapture = a_create_capture,
  .DestroyCapture = a_destroy_capture,
  .GetPortCapability = a_port_capability,
  .SetPassthroughMode = a_set_passthrough,
  .GetPassthroughMode = a_get_passthrough,
  .GetDeviceStatus = a_device_status,
  .UpdateAudioRoute = a_update_route,
  .ReleaseAudioRoute = a_release_route,
  .SetMicMute = a_set_mic_mute,
  .GetMicMute = a_get_mic_mute,
  .SetVoiceVolume = a_voice_volume,
  .SetExtraParams = a_set_extra,
  .GetExtraParams = a_get_extra,
};

// ---- manager ----

// The adapter frees the names and ports with free().
static int32_t m_adapters(
  struct IAudioManagerVdi* m, struct AudioAdapterDescriptorVdi* descs,
  uint32_t* len
) {
  (void)m;
  if (!descs || !len || *len < 1) return HDF_ERR_INVALID_PARAM;
  struct AudioPortVdi* ports = calloc(2, sizeof(*ports));
  char* name = strdup(ADAPTER_NAME);
  char* out = strdup("AOP");
  char* in = strdup("AIP");
  if (!ports || !name || !out || !in) {
    free(ports);
    free(name);
    free(out);
    free(in);
    return HDF_FAILURE;
  }
  ports[0] = (struct AudioPortVdi){PORT_VDI_OUT, OUT_PORT, out};
  ports[1] = (struct AudioPortVdi){PORT_VDI_IN, IN_PORT, in};
  descs[0] = (struct AudioAdapterDescriptorVdi){name, ports, 2};
  *len = 1;
  return HDF_SUCCESS;
}

static int32_t m_load(
  struct IAudioManagerVdi* m, const struct AudioAdapterDescriptorVdi* desc,
  struct IAudioAdapterVdi** adapter
) {
  (void)m;
  if (!desc || !desc->adapterName || !adapter) return HDF_ERR_INVALID_PARAM;
  if (strcmp(desc->adapterName, ADAPTER_NAME) != 0) return HDF_ERR_NOT_SUPPORT;
  *adapter = &g_adapter;
  return HDF_SUCCESS;
}

static int32_t m_unload(
  struct IAudioManagerVdi* m, struct IAudioAdapterVdi* a
) {
  (void)m;
  return a == &g_adapter ? HDF_SUCCESS : HDF_ERR_INVALID_PARAM;
}

static int32_t m_release(struct IAudioManagerVdi* m) {
  (void)m;
  return HDF_SUCCESS;
}

static struct IAudioManagerVdi g_manager = {
  .GetAllAdapters = m_adapters,
  .LoadAdapter = m_load,
  .UnloadAdapter = m_unload,
  .ReleaseAudioManagerObject = m_release,
};

EXPORT struct IAudioManagerVdi* AudioManagerCreateIfInstance(void) {
  return &g_manager;
}
