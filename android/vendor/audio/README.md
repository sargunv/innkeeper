# audio

Speaker playback uses our AIDL `IModule/innkeeper` and `StreamBridge`, over the
shared bridge client and the bridge's PipeWire output. The service and rc are an
overlay in `/apex/com.android.hardware.audio/`; the image stays unmodified. Its
frozen NDK AIDL bindings are generated at configure time with the pinned
build-tools, like the graphics HAL.

Microphone capture uses a separate `StreamInBridge` in the same module, through
our existing `libtinyalsav2.so` capture backend. The image's default primary HAL
retains its microphone route as a fallback, plus telephony. The policy XML
removes its speaker, MMAP and offload ports; MMAP and offload were silent stubs.

- **One volume control.** Android's stream volumes stay at maximum
  (`config_useFixedVolume`): the desktop mixer is the only volume control, so an
  app slider and the mixer do not stack. An app's own per-player gain still
  applies.
- **Why an output module.** The image's primary HAL hardcodes an 85 ms nominal
  latency and a 4096-frame minimum buffer at 48 kHz. Its emulator workaround
  also skips transfers when uptime pacing falls behind. Smaller rings alone
  cannot change that contract. Our module offers a 240-frame / 5 ms burst and
  the `FAST` flag, so AudioFlinger can use FastMixer.
- **Mixer names.** `StreamOut::updateMetadata` reports the highest-priority
  usage among the active tracks (voice call, then game, media, assistant,
  navigation, notification or alarm, system sounds, unknown). An empty label
  lets the bridge derive `media.name` and `media.role`. The PCM backend sends
  the request only when that usage changes, including after a reconnect; an
  empty track list sends nothing. Capture stays unnamed: the bridge ignores
  `SET_METADATA` on record streams.
- **Pacing and timestamps.** `StreamBridge` reuses our PCM backend's blocking
  writes and presentation estimate. Ring space follows PipeWire's audio clock;
  there is no second uptime pacer and no deliberate skipped transfer.
  Presentation timestamps include both the ring and the downstream queue.
  Without the bridge the existing virtual clock and reconnect behavior apply.
- **Buffers.** The PCM backend uses two 240-frame periods (10 ms total). The
  bridge requests another 20 ms downstream, so the output port reports a 30 ms
  nominal latency, while stream replies report the observed queued frames.
  `audio.prop` sets AudioFlinger's normal playback period to 10 ms (480 frames),
  reducing the queue in its pipe to FastMixer. Capture periods are unchanged.
- **Capture.** Android's `RECORD_AUDIO` runtime permission is the consent. The
  bridge records the default Pulse/PipeWire source into a capture ring;
  `INNKEEPER_AUDIO_SOURCE` selects a source explicitly (tests use a null-sink
  monitor). No source, a stalled backend or a bridge restart returns paced
  silence while retrying once a second. Standby releases the host record stream;
  the next read opens a fresh ring. The Flatpak's existing `--socket=pulseaudio`
  grants playback and capture. The input profile is 48 kHz, s16, mono or stereo;
  AudioFlinger converts other app formats. Capture transfers use 480-frame / 10
  ms bursts directly to the framework FMQ, with no intermediate MonoPipe. The
  capture ring holds eight periods (80 ms) for scheduling jitter; reads return
  as soon as their requested samples arrive. Standby closes the PCM and discards
  the previous ring. Module microphone mute zeroes captured samples.
- **Why an input stream.** The stock HAL's blocking MonoPipe writes sleep for
  1.15 times the requested audio duration. Instrumented 4096-frame writes at 48
  kHz ran every 98.13 ms while RecordThread consumed every 85.33 ms, inserting
  silence. Explicit PCM pacing only moved that delay into `pcm_read`; smaller
  tinyalsa periods and a policy FAST hint left its fixed 4096-frame request
  unchanged. Even a 262144-frame capture ring with no overruns left empty pipe
  reads. A direct AIDL input transfer removes that throttle while reusing the
  same tinyalsa client, reconnect and silence behavior. The original microphone
  remains attached with address `bottom`; the bridge microphone's empty address
  is selected by default. Selecting the original device exercises the stock HAL
  fallback, which still has its pipe limitation.
- **Capture overrides.** `audio.prop` keeps the original HAL at two periods. Its
  rc preloads the session shim to make priority changes best effort in the
  rootless session. Output does not require that workaround.

`mise run build:audio` builds and stages both services' artifacts. Host unit
checks cover output buffer sizes, position and latency arithmetic, and FMQ
wrapping and wakeups. `tests/abi.sh` checks the tinyalsa exports; the audio
group of `mise run test:e2e` plays a 440 Hz tone into a private null sink and
checks its frequency, then detects a known marker through AudioRecord from that
sink's monitor. `tests/e2e/audio/capture.sh` verifies 30 s of continuous 440 Hz
capture against a simultaneous native control. `mise run bench -- audio`
measures app-visible output latency and software capture round trip.
