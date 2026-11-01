// SPDX-License-Identifier: AGPL-3.0-or-later
// The audio VDI through the tables that OH's adapter calls. The host has no
// bridge socket, so renders run on pcm.h's virtual clock and captures return
// paced silence.
#include <assert.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "../vdi.h"

struct IAudioManagerVdi* AudioManagerCreateIfInstance(void);

static int64_t now_ns(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static struct AudioSampleAttributesVdi attrs(uint32_t rate, uint32_t ch) {
  return (struct AudioSampleAttributesVdi){
    .format = AUDIO_VDI_FORMAT_TYPE_PCM_16_BIT,
    .interleaved = true,
    .sampleRate = rate,
    .channelCount = ch,
    .isSignedData = true,
  };
}

static struct IAudioAdapterVdi* load(struct IAudioManagerVdi* m) {
  struct AudioAdapterDescriptorVdi descs[4];
  uint32_t len = 0;
  assert(m->GetAllAdapters(m, descs, &len) == HDF_ERR_INVALID_PARAM);
  len = 4;
  assert(m->GetAllAdapters(m, descs, &len) == HDF_SUCCESS);
  assert(len == 1 && strcmp(descs[0].adapterName, "primary") == 0);
  assert(descs[0].portsLen == 2);
  assert(descs[0].ports[0].dir == PORT_VDI_OUT);
  assert(descs[0].ports[1].dir == PORT_VDI_IN);
  assert(descs[0].ports[0].portId != descs[0].ports[1].portId);

  struct IAudioAdapterVdi* a = NULL;
  struct AudioAdapterDescriptorVdi usb = {.adapterName = "usb"};
  assert(m->LoadAdapter(m, &usb, &a) == HDF_ERR_NOT_SUPPORT);
  assert(m->LoadAdapter(m, &descs[0], &a) == HDF_SUCCESS && a);
  assert(a->InitAllPorts(a) == HDF_SUCCESS);

  struct AudioPortCapabilityVdi cap;
  assert(a->GetPortCapability(a, &descs[0].ports[0], &cap) == HDF_SUCCESS);
  assert(
    cap.formatNum == 1 && cap.formats[0] == AUDIO_VDI_FORMAT_TYPE_PCM_16_BIT
  );
  struct AudioPortVdi other = {.dir = PORT_VDI_OUT, .portId = 9};
  assert(a->GetPortCapability(a, &other, &cap) == HDF_ERR_NOT_SUPPORT);
  int32_t route = 0;
  assert(
    a->UpdateAudioRoute(a, (const struct AudioRouteVdi*)&route, &route) ==
    HDF_SUCCESS
  );

  for (uint32_t i = 0; i < len; i++) {
    for (uint32_t p = 0; p < descs[i].portsLen; p++)
      free(descs[i].ports[p].portName);
    free(descs[i].ports);
    free(descs[i].adapterName);
  }
  return a;
}

static void unsupported_formats(struct IAudioAdapterVdi* a) {
  struct AudioDeviceDescriptorVdi dev = {.pins = 1};
  struct IAudioRenderVdi* r = NULL;
  struct AudioSampleAttributesVdi bad = attrs(48000, 3);
  assert(a->CreateRender(a, &dev, &bad, &r) == HDF_ERR_NOT_SUPPORT);
  bad = attrs(48000, 2);
  bad.format = AUDIO_VDI_FORMAT_TYPE_PCM_16_BIT | 1;  // 24-bit
  assert(a->CreateRender(a, &dev, &bad, &r) == HDF_ERR_NOT_SUPPORT);
  bad = attrs(48000, 2);
  bad.isBigEndian = true;
  assert(a->CreateRender(a, &dev, &bad, &r) == HDF_ERR_NOT_SUPPORT);
  bad = attrs(4000, 2);
  struct IAudioCaptureVdi* c = NULL;
  assert(a->CreateCapture(a, &dev, &bad, &c) == HDF_ERR_NOT_SUPPORT);
}

static void render(struct IAudioAdapterVdi* a) {
  struct AudioDeviceDescriptorVdi dev = {.pins = 1};
  struct AudioSampleAttributesVdi at = attrs(48000, 2);
  struct IAudioRenderVdi *r = NULL, *r2 = NULL;
  assert(a->CreateRender(a, &dev, &at, &r) == HDF_SUCCESS);
  assert(a->CreateRender(a, &dev, &at, &r2) == HDF_SUCCESS);
  // OH's adapter clears entries in each render's table.
  r->GetFrameBufferSize = NULL;
  assert(r2->GetFrameBufferSize);

  uint32_t ms;
  assert(r->GetLatency(r, &ms) == HDF_SUCCESS && ms == 30);
  struct AudioSampleAttributesVdi got;
  assert(r->GetSampleAttributes(r, &got) == HDF_SUCCESS);
  assert(got.sampleRate == 48000 && got.channelCount == 2);
  assert(got.frameSize == 4 && got.period == 240);
  struct AudioSampleAttributesVdi other = attrs(44100, 2);
  assert(r->SetSampleAttributes(r, &other) == HDF_ERR_NOT_SUPPORT);
  assert(r->SetSampleAttributes(r, &at) == HDF_SUCCESS);

  // The device takes 100 ms of 5 ms bursts at the nominal rate. Its 10 ms
  // ring lets the writer run that far ahead.
  static int16_t burst[240 * 2];
  uint64_t reply = 0;
  int64_t start = now_ns();
  for (int i = 0; i < 20; i++) {
    assert(r->RenderFrame(r, (int8_t*)burst, sizeof(burst) + 3, &reply) == 0);
    assert(reply == sizeof(burst));
  }
  int64_t took = now_ns() - start;
  assert(took >= 85000000 && took < 400000000);
  uint64_t count, frames;
  assert(r->GetFrameCount(r, &count) == HDF_SUCCESS && count == 20 * 240);
  struct AudioTimeStampVdi t1, t2;
  assert(r->GetRenderPosition(r, &frames, &t1) == HDF_SUCCESS);
  assert(frames <= count && frames + 2 * 480 >= count);
  int64_t ns1 = t1.tvSec * 1000000000 + t1.tvNSec;
  assert(ns1 >= start && ns1 <= now_ns());
  // Queued frames drain on the device's clock.
  struct timespec wait = {0, 30000000};
  nanosleep(&wait, NULL);
  assert(r->GetRenderPosition(r, &frames, &t2) == HDF_SUCCESS);
  assert(frames == count);

  // Volume is reported but not applied. Out-of-range volumes are refused.
  float v;
  assert(r->SetVolume(r, 0.25f) == HDF_SUCCESS);
  assert(r->GetVolume(r, &v) == HDF_SUCCESS && v == 0.25f);
  assert(r->SetVolume(r, 2) == HDF_ERR_INVALID_PARAM);
  bool mute;
  assert(r->SetMute(r, true) == HDF_SUCCESS);
  assert(r->GetMute(r, &mute) == HDF_SUCCESS && mute);
  assert(r->RenderFrame(r, (int8_t*)burst, sizeof(burst), &reply) == 0);
  assert(r->Stop(r) == HDF_SUCCESS);
  assert(r->GetFrameCount(r, &count) == HDF_SUCCESS && count == 21 * 240);
  assert(a->DestroyRender(a, r) == HDF_SUCCESS);
  assert(a->DestroyRender(a, r2) == HDF_SUCCESS);
}

static void capture(struct IAudioAdapterVdi* a) {
  struct AudioDeviceDescriptorVdi dev = {.pins = PIN_VDI_IN_MIC};
  struct AudioSampleAttributesVdi at = attrs(48000, 1);
  struct IAudioCaptureVdi* c = NULL;
  assert(a->CreateCapture(a, &dev, &at, &c) == HDF_SUCCESS);
  assert(a->SetMicMute(a, true) == HDF_SUCCESS);
  bool mute = false;
  assert(c->GetMute(c, &mute) == HDF_SUCCESS && mute);
  assert(a->SetMicMute(a, false) == HDF_SUCCESS);

  static int16_t buf[480];
  uint32_t len = sizeof(buf) + 1;
  uint64_t reply = 0;
  int64_t start = now_ns();
  for (int i = 0; i < 3; i++) {
    memset(buf, 0x55, sizeof(buf));
    assert(c->CaptureFrame(c, (int8_t*)buf, &len, &reply) == HDF_SUCCESS);
    assert(reply == sizeof(buf));
    for (int j = 0; j < 480; j++) assert(buf[j] == 0);
  }
  assert(now_ns() - start >= 30000000);
  uint64_t frames;
  struct AudioTimeStampVdi t;
  assert(c->GetCapturePosition(c, &frames, &t) == HDF_SUCCESS);
  assert(frames == 3 * 480);
  assert(c->Stop(c) == HDF_SUCCESS);
  assert(a->DestroyCapture(a, c) == HDF_SUCCESS);
}

int main(void) {
  struct IAudioManagerVdi* m = AudioManagerCreateIfInstance();
  struct IAudioAdapterVdi* a = load(m);
  unsupported_formats(a);
  render(a);
  capture(a);
  assert(m->UnloadAdapter(m, a) == HDF_SUCCESS);
  assert(m->ReleaseAudioManagerObject(m) == HDF_SUCCESS);
  return 0;
}
