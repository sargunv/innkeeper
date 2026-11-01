# audio

This directory holds our AIDL audio module, `IModule/innkeeper`. It is Android's
playback and capture path, and it carries audio through the bridge's PipeWire
streams. `StreamBridge` handles output and `StreamInBridge` handles input. Both
use the bridge's shared rings through `pcm.c`, a blocking PCM device over the
audio client in `audio.c`. Both are in
[guest/audio-client](../../../guest/audio-client/include/innkeeper/pcm.h). The
service and its rc are an overlay in `/apex/com.android.hardware.audio/`, and
the image is unmodified. As in the graphics HALs, the frozen NDK AIDL bindings
are generated at configure time with the pinned build-tools.

The image's example HAL still runs for `IConfig`, remote submix, Bluetooth and
effects. The policy XML empties its primary module, because its ALSA microphone,
telephony, FM, MMAP and offload ports have no device in the session. Our
microphone is the only built-in input.

- **Volume.** Android's stream volumes stay at maximum
  (`config_useFixedVolume`), so an app's volume slider and the desktop mixer
  don't stack. An app's own per-player gain still applies.
- **Output stream.** The stock HAL hardcodes an 85 ms nominal latency and a
  4096-frame minimum buffer at 48 kHz. Its emulator workaround also skips
  transfers when uptime pacing falls behind. Our module offers a 240-frame (5
  ms) burst and the `FAST` flag, so AudioFlinger can use FastMixer.
- **Input stream.** In the stock HAL, each MonoPipe write sleeps for 1.15 times
  the audio duration. A 4096-frame request arrived every 98 ms while
  RecordThread consumed one every 85 ms, so the gaps became silence. No period
  or ring size changes that request size. `StreamInBridge` transfers 480-frame
  (10 ms) bursts directly to the framework FMQ.
- **Pacing and timestamps.** Writes block on ring space, which follows
  PipeWire's audio clock. There is no uptime pacer and no skipped transfer.
  Presentation timestamps include the ring and the downstream queue.
- **Buffers.** Playback uses two 240-frame periods (10 ms), and the bridge
  requests another 20 ms downstream. The output port reports 30 ms of nominal
  latency, and stream replies report the observed queued frames. `audio.prop`
  sets AudioFlinger's normal playback period to 10 ms (480 frames), which
  shrinks its pipe to FastMixer. The capture ring holds eight periods (80 ms) to
  absorb scheduling jitter. Reads return as soon as their samples arrive.
- **Mixer names.** `StreamOut::updateMetadata` reports the highest-priority
  usage among the active tracks with an empty label. The priority order is voice
  call, game, media, assistant, navigation, notification or alarm, system
  sounds, and unknown. The bridge derives `media.name` and `media.role` from
  that usage. `pcm.c` sends the usage when it changes and again after a
  reconnect. Capture streams have no name, because the bridge ignores
  `SET_METADATA` on record streams.
- **Capture.** Android's `RECORD_AUDIO` permission is the user's consent. The
  Flatpak's `--socket=pulseaudio` grants both playback and capture. The bridge
  records the default source, or `INNKEEPER_AUDIO_SOURCE` if it is set. Tests
  set it to a null-sink monitor. The input profile is 48 kHz s16, mono or
  stereo, and AudioFlinger converts other formats. The module's microphone mute
  zeroes the captured samples. Standby closes the PCM and the host record
  stream, and the next read opens a fresh ring.
- **No bridge.** Without a bridge, or after a stall or restart, output runs on a
  virtual clock and input returns paced silence. Both retry once a second.
- **Stock HAL rc.** The stock HAL's rc preloads the session shim. The shim makes
  the priority changes of its stream threads (remote submix) best effort in the
  rootless session. Our service doesn't need the shim.

`mise run build:ndk` builds and stages the artifacts of both services. Host unit
tests cover output buffer sizes, position and latency arithmetic, usage
priority, and FMQ wrapping and wakeups. The audio client's tests cover paced
capture without a bridge, capture underruns, partial reads and bridge death. The
audio group of `mise run test:e2e` plays a 440 Hz tone into a private null sink
and checks its frequency. It then reads that sink's monitor through AudioRecord
and detects a known marker.
