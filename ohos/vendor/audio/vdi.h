// SPDX-License-Identifier: AGPL-3.0-or-later
// OpenHarmony's primary audio VDI, laid out as OH 7.0 builds it. The types
// come from drivers_peripheral's audio/interfaces/sound/v1_0 headers. The
// image's IDL-to-VDI adapter (libaudio_primary_impl_vdi.z.so) calls these
// tables, and their order and the attribute offsets match its code.
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define HDF_SUCCESS 0
#define HDF_FAILURE (-1)
#define HDF_ERR_NOT_SUPPORT (-2)
#define HDF_ERR_INVALID_PARAM (-3)

enum { PORT_VDI_OUT = 1, PORT_VDI_IN = 2 };
enum { PIN_VDI_NONE = 0, PIN_VDI_IN_MIC = 1 << 27 | 1 << 0 };
enum { AUDIO_VDI_FORMAT_TYPE_PCM_16_BIT = 1 << 1 };
enum { AUDIO_VDI_CHANNEL_STEREO = 3, AUDIO_VDI_CHANNEL_MONO = 4 };
enum { AUDIO_VDI_SAMPLE_RATE_MASK_44100 = 1 << 7 };
enum { AUDIO_VDI_SAMPLE_RATE_MASK_48000 = 1 << 8 };
enum { AUDIO_VDI_SAMPLE_FORMAT_S16 = 4 };
enum { AUDIO_VDI_PORT_PASSTHROUGH_LPCM = 1 };
enum { AUDIO_VDI_PRIMARY_DEVICE = 1 << 7 };

struct AudioPortVdi {
  int32_t dir;  // AudioPortDirectionVdi
  uint32_t portId;
  char* portName;
};

struct AudioAdapterDescriptorVdi {
  char* adapterName;
  struct AudioPortVdi* ports;
  uint32_t portsLen;
};

struct AudioDeviceDescriptorVdi {
  uint32_t portId;
  int32_t pins;  // AudioPortPinVdi
  char* desc;
};

struct AudioSceneDescriptorVdi {
  union {
    uint32_t id;
  } __attribute__((aligned(8))) scene;
  struct AudioDeviceDescriptorVdi desc;
};

// The VDI only reads and writes the leading PCM fields. The offload, echo
// and raw attributes that follow stay opaque.
struct AudioSampleAttributesVdi {
  int32_t type;  // AudioCategoryVdi
  bool interleaved;
  int32_t format;  // AudioFormatVdi
  uint32_t sampleRate;
  uint32_t channelCount;
  uint64_t channelLayout;
  uint32_t period;
  uint32_t frameSize;
  bool isBigEndian;
  bool isSignedData;
  uint32_t startThreshold;
  uint32_t stopThreshold;
  uint32_t silenceThreshold;
  int32_t streamId;
  int32_t sourceType;
};
_Static_assert(
  offsetof(struct AudioSampleAttributesVdi, channelLayout) == 0x18,
  "OH 7.0 layout"
);
_Static_assert(
  offsetof(struct AudioSampleAttributesVdi, isBigEndian) == 0x28,
  "OH 7.0 layout"
);
_Static_assert(
  offsetof(struct AudioSampleAttributesVdi, sourceType) == 0x3c, "OH 7.0 layout"
);

struct AudioTimeStampVdi {
  int64_t tvSec;
  int64_t tvNSec;
};

struct AudioSubPortCapabilityVdi {
  uint32_t portId;
  char* desc;
  int32_t mask;  // AudioPortPassthroughModeVdi
};

struct AudioPortCapabilityVdi {
  uint32_t deviceType;
  uint32_t deviceId;
  bool hardwareMode;
  uint32_t formatNum;
  int32_t* formats;  // AudioFormatVdi
  uint32_t formatsLen;
  uint32_t sampleRateMasks;
  int32_t channelMasks;  // AudioChannelMaskVdi
  uint32_t channelCount;
  struct AudioSubPortCapabilityVdi* subPorts;
  uint32_t subPortsLen;
  int32_t* supportSampleFormats;  // AudioSampleFormatVdi
  uint32_t supportSampleFormatsLen;
};

struct AudioDeviceStatusVdi {
  uint32_t pnpStatus;
} __attribute__((aligned(8)));

struct AudioRouteVdi;
struct AudioMmapBufferDescriptorVdi;
struct AudioCaptureFrameInfoVdi;
struct IAudioCallbackVdi;

typedef void* VdiFn;  // an entry the VDI leaves NULL

struct IAudioRenderVdi {
  int32_t (*GetLatency)(struct IAudioRenderVdi*, uint32_t* ms);
  int32_t (*RenderFrame)(
    struct IAudioRenderVdi*, const int8_t* frame, uint32_t len, uint64_t* reply
  );
  int32_t (*GetRenderPosition)(
    struct IAudioRenderVdi*, uint64_t* frames, struct AudioTimeStampVdi* time
  );
  VdiFn SetRenderSpeed, GetRenderSpeed, SetChannelMode, GetChannelMode;
  VdiFn RegCallback, DrainBuffer;
  int32_t (*IsSupportsDrain)(struct IAudioRenderVdi*, bool* support);
  int32_t (*CheckSceneCapability)(
    struct IAudioRenderVdi*, const struct AudioSceneDescriptorVdi*, bool* ok
  );
  int32_t (*SelectScene)(
    struct IAudioRenderVdi*, const struct AudioSceneDescriptorVdi*
  );
  int32_t (*SetMute)(struct IAudioRenderVdi*, bool mute);
  int32_t (*GetMute)(struct IAudioRenderVdi*, bool* mute);
  int32_t (*SetVolume)(struct IAudioRenderVdi*, float volume);
  int32_t (*SetVolumeWithRamp)(struct IAudioRenderVdi*, float, uint32_t);
  int32_t (*GetVolume)(struct IAudioRenderVdi*, float* volume);
  int32_t (*GetGainThreshold)(struct IAudioRenderVdi*, float* min, float* max);
  int32_t (*GetGain)(struct IAudioRenderVdi*, float* gain);
  int32_t (*SetGain)(struct IAudioRenderVdi*, float gain);
  int32_t (*GetFrameSize)(struct IAudioRenderVdi*, uint64_t* size);
  int32_t (*GetFrameCount)(struct IAudioRenderVdi*, uint64_t* count);
  int32_t (*SetSampleAttributes)(
    struct IAudioRenderVdi*, const struct AudioSampleAttributesVdi*
  );
  int32_t (*GetSampleAttributes)(
    struct IAudioRenderVdi*, struct AudioSampleAttributesVdi*
  );
  int32_t (*GetCurrentChannelId)(struct IAudioRenderVdi*, uint32_t* id);
  int32_t (*SetExtraParams)(struct IAudioRenderVdi*, const char* kv);
  int32_t (*GetExtraParams)(struct IAudioRenderVdi*, char* kv, uint32_t len);
  VdiFn ReqMmapBuffer, GetMmapPosition, AddAudioEffect, RemoveAudioEffect;
  int32_t (*GetFrameBufferSize)(struct IAudioRenderVdi*, uint64_t* size);
  int32_t (*Start)(struct IAudioRenderVdi*);
  int32_t (*Stop)(struct IAudioRenderVdi*);
  int32_t (*Pause)(struct IAudioRenderVdi*);
  int32_t (*Resume)(struct IAudioRenderVdi*);
  int32_t (*Flush)(struct IAudioRenderVdi*);
  int32_t (*TurnStandbyMode)(struct IAudioRenderVdi*);
  int32_t (*AudioDevDump)(struct IAudioRenderVdi*, int32_t range, int32_t fd);
  int32_t (*IsSupportsPauseAndResume)(
    struct IAudioRenderVdi*, bool* pause, bool* resume
  );
  int32_t (*SetBufferSize)(struct IAudioRenderVdi*, uint32_t size);
};
_Static_assert(
  offsetof(struct IAudioRenderVdi, Start) == 0x100, "OH 7.0 layout"
);

struct IAudioCaptureVdi {
  int32_t (*CaptureFrame)(
    struct IAudioCaptureVdi*, int8_t* frame, uint32_t* len, uint64_t* reply
  );
  VdiFn CaptureFrameEc;
  int32_t (*GetCapturePosition)(
    struct IAudioCaptureVdi*, uint64_t* frames, struct AudioTimeStampVdi* time
  );
  int32_t (*CheckSceneCapability)(
    struct IAudioCaptureVdi*, const struct AudioSceneDescriptorVdi*, bool* ok
  );
  int32_t (*SelectScene)(
    struct IAudioCaptureVdi*, const struct AudioSceneDescriptorVdi*
  );
  int32_t (*SetMute)(struct IAudioCaptureVdi*, bool mute);
  int32_t (*GetMute)(struct IAudioCaptureVdi*, bool* mute);
  int32_t (*SetVolume)(struct IAudioCaptureVdi*, float volume);
  int32_t (*GetVolume)(struct IAudioCaptureVdi*, float* volume);
  int32_t (*GetGainThreshold)(struct IAudioCaptureVdi*, float* min, float* max);
  int32_t (*GetGain)(struct IAudioCaptureVdi*, float* gain);
  int32_t (*SetGain)(struct IAudioCaptureVdi*, float gain);
  int32_t (*GetFrameSize)(struct IAudioCaptureVdi*, uint64_t* size);
  int32_t (*GetFrameCount)(struct IAudioCaptureVdi*, uint64_t* count);
  int32_t (*SetSampleAttributes)(
    struct IAudioCaptureVdi*, const struct AudioSampleAttributesVdi*
  );
  int32_t (*GetSampleAttributes)(
    struct IAudioCaptureVdi*, struct AudioSampleAttributesVdi*
  );
  int32_t (*GetCurrentChannelId)(struct IAudioCaptureVdi*, uint32_t* id);
  int32_t (*SetExtraParams)(struct IAudioCaptureVdi*, const char* kv);
  int32_t (*GetExtraParams)(struct IAudioCaptureVdi*, char* kv, uint32_t len);
  VdiFn ReqMmapBuffer, GetMmapPosition, AddAudioEffect, RemoveAudioEffect;
  VdiFn GetFrameBufferSize;
  int32_t (*Start)(struct IAudioCaptureVdi*);
  int32_t (*Stop)(struct IAudioCaptureVdi*);
  int32_t (*Pause)(struct IAudioCaptureVdi*);
  int32_t (*Resume)(struct IAudioCaptureVdi*);
  int32_t (*Flush)(struct IAudioCaptureVdi*);
  int32_t (*TurnStandbyMode)(struct IAudioCaptureVdi*);
  int32_t (*AudioDevDump)(struct IAudioCaptureVdi*, int32_t range, int32_t fd);
  VdiFn IsSupportsPauseAndResume, RegCallback;
};
_Static_assert(
  offsetof(struct IAudioCaptureVdi, Start) == 0xc0, "OH 7.0 layout"
);

struct IAudioAdapterVdi {
  int32_t (*InitAllPorts)(struct IAudioAdapterVdi*);
  int32_t (*CreateRender)(
    struct IAudioAdapterVdi*, const struct AudioDeviceDescriptorVdi*,
    const struct AudioSampleAttributesVdi*, struct IAudioRenderVdi**
  );
  int32_t (*DestroyRender)(struct IAudioAdapterVdi*, struct IAudioRenderVdi*);
  int32_t (*CreateCapture)(
    struct IAudioAdapterVdi*, const struct AudioDeviceDescriptorVdi*,
    const struct AudioSampleAttributesVdi*, struct IAudioCaptureVdi**
  );
  int32_t (*DestroyCapture)(struct IAudioAdapterVdi*, struct IAudioCaptureVdi*);
  int32_t (*GetPortCapability)(
    struct IAudioAdapterVdi*, const struct AudioPortVdi*,
    struct AudioPortCapabilityVdi*
  );
  int32_t (*SetPassthroughMode)(
    struct IAudioAdapterVdi*, const struct AudioPortVdi*, int32_t mode
  );
  int32_t (*GetPassthroughMode)(
    struct IAudioAdapterVdi*, const struct AudioPortVdi*, int32_t* mode
  );
  int32_t (*GetDeviceStatus)(
    struct IAudioAdapterVdi*, struct AudioDeviceStatusVdi*
  );
  int32_t (*UpdateAudioRoute)(
    struct IAudioAdapterVdi*, const struct AudioRouteVdi*, int32_t* handle
  );
  int32_t (*ReleaseAudioRoute)(struct IAudioAdapterVdi*, int32_t handle);
  int32_t (*SetMicMute)(struct IAudioAdapterVdi*, bool mute);
  int32_t (*GetMicMute)(struct IAudioAdapterVdi*, bool* mute);
  int32_t (*SetVoiceVolume)(struct IAudioAdapterVdi*, float volume);
  int32_t (*SetExtraParams)(
    struct IAudioAdapterVdi*, int32_t key, const char* cond, const char* value
  );
  int32_t (*GetExtraParams)(
    struct IAudioAdapterVdi*, int32_t key, const char* cond, char* value,
    uint32_t len
  );
  VdiFn RegExtraParamObserver, CreateCognitionStream, DestroyCognitionStream;
  VdiFn NotifyCognitionData, CreateCallTransfer, SetPhoneCallScene;
  int32_t (*CheckSupport)(
    struct IAudioAdapterVdi*, const struct AudioDeviceDescriptorVdi*
  );
};

struct IAudioManagerVdi {
  int32_t (*GetAllAdapters)(
    struct IAudioManagerVdi*, struct AudioAdapterDescriptorVdi* descs,
    uint32_t* len
  );
  int32_t (*LoadAdapter)(
    struct IAudioManagerVdi*, const struct AudioAdapterDescriptorVdi*,
    struct IAudioAdapterVdi**
  );
  int32_t (*UnloadAdapter)(struct IAudioManagerVdi*, struct IAudioAdapterVdi*);
  int32_t (*ReleaseAudioManagerObject)(struct IAudioManagerVdi*);
};
