# Audio (M2)

Status: Android plays to PipeWire on the emulator image, proven end to end (440
Hz tone, game music). The CI image needs its APEX overlay; see "Open issues".
The bridge and its protocol are documented in [bridge.md](bridge.md).

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

## How it fits together

```
 Android session                                         host (glibc)
 ┌──────────────────────────────────────────────┐       ┌──────────────────────────────┐
 │ app → AudioTrack/AAudio → AudioFlinger       │       │ linuxapk-bridge (Rust)       │
 │   → audio HAL (AOSP AIDL default, or         │ socket│   audio service: one thread  │
 │     goldfish HIDL on the emulator image)     │◄─────►│   per stream                 │
 │   → libtinyalsav2.so (ours: audio/tinyalsa)  │ memfd │   ring → Pulse protocol      │
 │       card 0 "Linux.apk": pcm_write → ring ──┼──────►│   → pipewire-pulse → sink    │
 └──────────────────────────────────────────────┘ +efd  └──────────────────────────────┘
```

- **`audio/tinyalsa/`** (C, NDK, LL-NDK deps only: libc, libdl, liblog) builds
  `libtinyalsav2.so`, exporting the same 89 symbols as the image's copy
  (`audio/build.sh` checks this). It has three parts:
  - `pcm.c`: the PCM API.
  - `mixer.c`: "Master Playback Volume/Switch" forwarded as SET_MASTER, plus
    stored capture controls.
  - `bridge_client.c`: the protocol client and ring writer. It has no tinyalsa
    types, so an AIDL stream can reuse it.
- **PCM behaviour:**
  - The bridge stream opens lazily on the first write, so the HAL's open/close
    capability probes cost nothing.
  - Blocking writes wait for ring space, so the host's audio clock paces the
    HAL.
  - `PCM_NONBLOCK` (goldfish) behaves like ALSA: it takes what fits, returns
    `-EAGAIN`, and the poll fd is POLLOUT-ready only while there's room.
  - `pcm_get_htimestamp`/`pcm_get_delay` come from the bridge's presentation
    estimate, which includes PipeWire's queue and the device. The reported
    buffer has 200 ms of headroom, so alsa_utils' "written − (buffer − avail)"
    is the presented position.
  - Without a bridge, writes are paced by `CLOCK_MONOTONIC` and dropped. It
    reconnects once a second, and positions stay continuous across rings.
- **Capture** (`PCM_IN`) returns silence at the nominal rate; see "Open issues".
- **Config:**
  - `audio/etc/audio.prop` goes into the vendor overlay, and the launcher merges
    it into `/vendor/build.prop`, the same way as `gpu.prop`. It sets the
    tinyalsa period count to 2 and the multiplier to 1, and removes the
    emulator's 80 ms "host latency".
  - The image's audio policy config already maps the primary module to Speaker
    and Built-In Mic, so we don't overlay it.
- **The bridge** (`bridge/`, Rust) plays each output stream as one PipeWire
  stream through PipeWire's Pulse protocol server, using the pure Rust
  `pulseaudio` crate. In the desktop mixer it appears as application
  "Linux.apk", media "Android".
  - Why not `libpipewire`: there are no PipeWire headers on the host and we
    install no `-devel` packages. The pure-Rust `pipewire-native` crate has no
    stream support yet. pipewire-pulse is the stable path most desktop apps use.
  - This way the bridge has no C dependencies and builds and lints like any
    crate.
- **Volume:** Android applies stream and track volumes in its mixer. Master
  volume and mute reach PipeWire as the stream's volume and mute.

## Running and testing

```sh
audio/build.sh                 # NDK build → out/vendor-overlay (+ out/audio-build/linuxapk-tone)
cargo test -p linuxapk-bridge  # protocol, ring, live server, fuzzing
audio/tests/e2e.sh             # session + null sink: 440 Hz tone + game capture
LINUXAPK_AUDIO_SINK=<sink> session/linuxapk-session start   # route to one sink
```

`e2e.sh` loads a private null sink, `linuxapk_test`, so nothing plays on the
speakers. It records the sink's monitor with `parec`. `freq.py` (pure Python
FFT) reports level and dominant frequency.

## Results (emulator image, 2026-10-27)

| Check                                          | Result                                                                                         |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| 440 Hz tone, `linuxapk-tone` (AAudio)          | captured **439.99 Hz** at −9.0 dBFS: the 0.5 sine is bit-exact through the chain at max volume |
| Game: Mindustry title music                    | **audible**: −35 dBFS over 10 s, dominant 61 Hz                                                |
| Bridge share of latency (host, null sink)      | ring fill ~15 ms + PipeWire downstream ~16 ms; a client sees **34 ms** written − presented     |
| App-visible output latency (AAudio timestamps) | **~93 ms** median (89–97) on the emulator image's goldfish HAL                                 |
| Bridge stats while playing                     | exactly 48 000 frames/s consumed, 0 underruns                                                  |

The emulator image misses the < 50 ms target. Goldfish's HIDL HAL runs its own
clock model and ring, with no fast mixer and no low-latency path (AAudio falls
back to `PERFORMANCE_MODE_NONE` with 44 ms buffers), and that sits on top of our
~34 ms. On the CI image, the AIDL HAL writes straight through alsa_utils into
our ring, so the expected total is AudioFlinger's buffering plus ~34 ms. That
gets measured once the CI session can load the overlay.

## Porting to AIDL (our source-built image, M4)

On our own image we don't need the tinyalsa trick: add a bridge stream to AOSP's
default AIDL HAL. `StreamCommonImpl` drivers implement `DriverInterface`, and
each call maps onto `bridge_client.h`:

| `DriverInterface` (`hardware/interfaces/audio/aidl/default`) | Bridge client                                                                                 |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------------- |
| `init()`                                                     | `lxb_open_output` (rate, channels and format from `StreamContext`; period = burst, 2 periods) |
| `transfer(buf, frames, &actual, &latencyMs)`                 | `lxb_write` (blocking); `latencyMs` = ring + `lxb_latency_ns`                                 |
| `refinePosition(&pos)`                                       | `lxb_presented` → observable frames and time                                                  |
| `standby()`, `pause()`                                       | `lxb_standby`                                                                                 |
| `flush()`                                                    | drop the ring (CLOSE, then reopen on next transfer)                                           |
| `drain(mode)`                                                | wait until `lxb_consumed == written`                                                          |
| `shutdown()`                                                 | `lxb_close_output`                                                                            |

- **Stream type:** a `StreamOutBridge`, returned from
  `ModulePrimary::createOutputStream` (or a module type "linuxapk"), about 200
  lines of C++. `Module::onMasterMuteChanged`/`onMasterVolumeChanged` call
  `lxb_set_master`.
- **Metadata:** `updateMetadataCommon` sends SET_METADATA with the dominant
  `AudioUsage`. The AIDL HAL sees the source metadata, so streams can be named
  by usage ("Game", "Media").
- **Build:** out of tree against the exported platform sysroot, or as a small
  AOSP patch with a written reason.

## Open issues

1. **The CI image needs the overlay at the right path.** Its HAL loads
   `libtinyalsav2.so` from `/apex/com.android.hardware.audio/lib64`. The symbol
   set matches (checked against the CI APEX), but the session needs an APEX
   overlay layer that `audio/build.sh` can install into. That's for the image
   lane, see `docs/image.md`. The proof (tone, game, latency) has to be rerun
   there.
2. **Per-app stream names.** The tinyalsa layer can't see which app is playing.
   AudioFlinger mixes every app into the primary output, so there is one
   PipeWire stream ("Linux.apk: Android"). For M3: the Linux.apk System app
   watches `AudioPlaybackConfiguration` and tells the bridge the active package,
   and the bridge renames the stream (SET_METADATA is already in the protocol).
3. **Microphone.** Capture is silence. It's cheap to add the same way (a Pulse
   record stream into a ring the HAL reads). But it opens the host microphone,
   so it should wait for a consent design (portal or session setting). Calls and
   voice apps need it.
4. **Shattered Pixel Dungeon plays no music.** Its Vorbis `MediaPlayer` reaches
   EOS about 0.35 s after each start, with "0 frames delivered" to its
   AudioTrack (`c2.android.vorbis.decoder ... received null buffer`). That's
   upstream of the HAL. The tone and Mindustry play fine, so this belongs in
   media/compat triage.
5. **Clock drift.** On the CI image, AOSP's `StreamPrimary` paces itself by
   `uptimeNanos` and skips a transfer when it falls behind, which is an emulator
   workaround. Against a device clock that is ppm off, expect a dropped buffer
   every few minutes. The AIDL port above removes this.
6. **Bridge restarts.** Streams fail over to the virtual clock and reconnect
   within 1 s (the ring is marked dead). Earlier audio in that window is lost.
