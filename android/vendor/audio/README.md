# audio

Speaker playback uses our AIDL `IModule/linuxapk` and `StreamBridge`, over the
shared bridge client and the bridge's PipeWire output. The service and rc are an
overlay in `/apex/com.android.hardware.audio/`; the image stays unmodified. Its
frozen NDK AIDL bindings are generated at configure time with the pinned
build-tools, like the graphics HAL.

The image's default primary HAL still handles capture and telephony through our
`libtinyalsav2.so`. The policy XML removes its speaker, MMAP and offload ports;
MMAP and offload were silent stubs. Keeping capture on the original module lets
its tinyalsa implementation evolve independently of the output stream.

- **Why an output module.** The image's primary HAL hardcodes an 85 ms nominal
  latency and a 4096-frame minimum buffer at 48 kHz. Its emulator workaround
  also skips transfers when uptime pacing falls behind. Smaller rings alone
  cannot change that contract. Our module offers a 240-frame / 5 ms burst and
  the `FAST` flag, so AudioFlinger can use FastMixer.
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
  `LINUXAPK_AUDIO_SOURCE` selects a source explicitly (tests use a null-sink
  monitor). No source, a stalled backend or a bridge restart returns paced
  silence while retrying once a second. Standby releases the host record stream;
  the next read opens a fresh ring. The Flatpak's existing `--socket=pulseaudio`
  grants playback and capture. Capture retains the HAL's requested buffer, since
  the source cannot wait for a reader. Sustained recording still has periodic
  gaps through the stock AOSP HAL's input pipe; see
  [#168](https://github.com/sargunv/linux.apk/issues/168).
- **Capture overrides.** `audio.prop` keeps the original HAL at two periods. Its
  rc preloads the session shim to make priority changes best effort in the
  rootless session. Output does not require that workaround.

`mise run build:audio` builds and stages both services' artifacts. Host unit
checks cover output buffer sizes, position and latency arithmetic, and FMQ
wrapping and wakeups. `tests/abi.sh` checks the tinyalsa exports; the audio
group of `mise run test:e2e` plays a 440 Hz tone into a private null sink and
checks its frequency, then detects a known marker through AudioRecord from that
sink's monitor. `mise run bench -- audio` measures app-visible output latency.
