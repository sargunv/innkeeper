# Audio (M2)

Status: in progress (audio lane). The protocol spec is in
[bridge.md](bridge.md).

## Decision: how Android's audio HAL reaches the host (2026-10-27)

The session is moving to the AOSP CI image (`aosp_cf_x86_64_only_phone`, build
16644040). Its vendor side has no HIDL audio wrapper. It runs AOSP's default
**AIDL** audio HAL (`android.hardware.audio.service-aidl.example` in the vendor
APEX `com.android.hardware.audio`). The HAL's primary module plays through
**tinyalsa** (`StreamPrimary` → `alsa_device_proxy` → `libtinyalsav2`, card 0
device 0). A legacy `audio.primary.*.so` would never be loaded there.

We considered three options:

- **(a) Our own AIDL `IModule`/stream HAL.** This is the cleanest boundary, but
  it means reimplementing ports, patches, configs, the FMQ stream state machine
  and `IConfig`. Building AOSP's default HAL out of tree drags in libfmq,
  libaudioaidlcommon, libmedia_helper, xsdc parsers and more. It's weeks of
  work, and our copy would drift from AOSP's version.
- **(b) Seccomp emulation of `/dev/snd/pcmC0D0p`.** This sits at the most stable
  layer (the kernel ALSA ioctl ABI), but every write becomes a supervisor round
  trip. It also needs the hw_params refine rules, status/control mmap or
  `SYNC_PTR`, and a supervisor that can block. And it shares a file with the
  ubinder lane's work.
- **(c) Chosen: replace the HAL's `libtinyalsav2.so`.** Our own implementation
  of the tinyalsa C API (`pcm_*`, `mixer_*`, BSD-licensed headers from AOSP 17)
  presents one virtual card, "Linux.apk". Its PCM device writes into a
  shared-memory ring that the host bridge plays to PipeWire.
  - The AOSP HAL keeps doing what it's good at: the AIDL state machine, routing,
    `IConfig`, effects and positions.
  - The library runs in-process, with no supervisor hops. A blocking `pcm_write`
    into the ring paces the HAL off the host's audio clock.
  - We overlay the file. Nothing is rebuilt, and nothing in the image is
    patched.
  - It works on both images. On the emulator image, the goldfish HIDL HAL also
    uses `libtinyalsav2` (from `/vendor/lib64`). That's where we prototype.
  - Mixer controls named "Master Playback Volume/Switch" carry Android's master
    volume and mute to the bridge.

  Trade-off: tinyalsa's C API is stable, but which library the HAL uses is not
  an Android boundary. If a future default HAL linked tinyalsa statically, or
  stopped using it, we would fall back to (a). The ring and bridge client are
  written so that a later AIDL `StreamBridge` (for our source-built image, see
  "Porting to AIDL" below) reuses them unchanged.

Install points:

- **CI image:** `/apex/com.android.hardware.audio/lib64/libtinyalsav2.so`,
  through the session's APEX overlay layer.
- **Emulator image:** `/vendor/lib64/libtinyalsav2.so`.
