# audio

Android's audio reaches PipeWire through our own `libtinyalsav2.so`, which
replaces the image's tinyalsa inside the AIDL audio HAL's APEX
(`/apex/com.android.hardware.audio/lib64/`). It presents one card, "Linux.apk",
whose PCM writes into a shared ring that the bridge's audio service plays
([host/bridge/src/audio](../../../host/bridge/src/audio/proto.rs)).
`mise run build:audio` builds it in the NDK superbuild, with the policy XML, the
HAL's rc override and `audio.prop`.

- **Why tinyalsa.** The image's AOSP default AIDL HAL plays through tinyalsa.
  Replacing that one library keeps the HAL's state machine, routing and
  positions, runs in-process with no supervisor hop, and patches nothing. Our
  own AIDL `IModule` would mean weeks of reimplementation that drifts from AOSP;
  emulating `/dev/snd` through seccomp would cost a round trip per write. If a
  future HAL stopped linking tinyalsa dynamically, `audio_service.c` (no
  tinyalsa types) is ready to back an AIDL stream instead.
- **Pacing.** Blocking writes wait for ring space, so the host's audio clock
  paces the HAL; without a bridge, writes are paced by `CLOCK_MONOTONIC` and
  dropped, and the stream reconnects once a second.
- **Overrides:** `primary_audio_policy_configuration.xml` drops the HAL's MMAP
  and offload ports, which it serves with silent stubs; the HAL's rc preloads
  the shim, because its stream threads fail on `setpriority` without
  `CAP_SYS_NICE`, which stalled boot; `audio.prop` sets 2 periods.
- **Latency** is about 86 ms as apps see it: our ring and PipeWire are about 34
  ms, the rest is the HAL's hardcoded 85 ms nominal latency. Getting under 50 ms
  needs our own stream in the HAL.

Tests: `tests/abi.sh` (in `mise run test:bionic`) checks that we export the
image library's symbols; the audio suite of `mise run test:e2e` plays a 440 Hz
AAudio tone into a private null sink and checks its frequency. Start reading at
`tinyalsa/pcm.c` and `tinyalsa/audio_service.c`.
