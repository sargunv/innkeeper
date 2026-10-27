# Audio (M2)

Status: Android plays to PipeWire, proven end to end on the emulator image (440
Hz tone, game music) and on the CI image (440 Hz tone, with the image lane's WIP
session). The CI image's install point lands with the image lane; see "Open
issues". The bridge and its protocol are documented in [bridge.md](bridge.md).

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

## Results (2026-10-27)

| Check                                          | Emulator image (goldfish HIDL HAL)                 | CI image (AOSP AIDL HAL)                                  |
| ---------------------------------------------- | -------------------------------------------------- | --------------------------------------------------------- |
| 440 Hz tone (`linuxapk-tone`, AAudio)          | **439.99 Hz**, −9.0 dBFS (bit-exact at max volume) | **439.99 Hz**, −9.0 dBFS, normal and LOW_LATENCY requests |
| Game: Mindustry title music                    | **audible**, −35 dBFS over 10 s                    | **audible**, −36 dBFS over 8 s                            |
| App-visible output latency (AAudio timestamps) | ~93 ms median (89–97)                              | **~86 ms** median (85–88)                                 |
| Our share: ring fill + PipeWire downstream     | ~15 + 16 ms                                        | 18.4 + 16 ms                                              |
| Bridge while playing                           | 48 000 frames/s, 0 underruns                       | 48 000 frames/s, 0 underruns                              |
| Compat corpus (`compat/run.sh`)                | 15/16 work, no regressions                         | (image lane's run)                                        |
| `selftest`                                     | 12/12                                              |                                                           |

Methods:

- **Tone:** `audio/tests/e2e.sh` (or the same steps by hand) plays 5 s through
  AAudio and records the null sink's monitor. Level and frequency come from
  `freq.py`.
- **App-visible latency:** Oboe's method,
  `(timestamp time + (written − position) / rate) − now`, sampled every 50 ms.
- **Our share:** the bridge's own statistics (`LINUXAPK_BRIDGE_VERBOSE=1`). The
  CI run used the image lane's WIP session, with our library in its cuttlefish
  APEX overlay.

**Why the < 50 ms target isn't met yet:**

- **Our share is ~34 ms.** The ring is capped at 2 × 10 ms whatever the HAL asks
  for. pipewire-pulse floors its own latency at ~16 ms (three 5.3 ms quanta),
  and 10 ms and 20 ms targets measured the same.
- **The rest is the HAL.**
  - AOSP's default AIDL HAL hardcodes an 85 ms nominal latency for its primary
    port (`ModulePrimary::getNominalLatencyMs`, "4096 frames … allows the
    virtual Android device implementation to pass CTS"). That sets
    AudioFlinger's HAL buffer.
  - Its only low-latency paths, the `mmap_no_irq_out` and `compressed_offload`
    ports, are served by silent stubs. We confirmed it: a LOW_LATENCY AAudio
    stream played nothing.
  - Setting `aaudio.mmap_policy=1` doesn't help, because AudioFlinger asks the
    AIDL HAL for its MMAP ports. Our policy overlay removes the ports instead.
    LOW_LATENCY streams then use the primary output: 439.99 Hz, 86 ms.
  - Goldfish (emulator image) adds its own clock-driven ring and has no fast
    mixer.
- **What gets us there:** our own stream in the AIDL HAL ("Porting to AIDL"
  below). It would offer a 10 ms primary or MMAP port backed by the ring, which
  should make it roughly 10 ms of mixer, 20 ms of ring and 16 ms of PipeWire.

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

1. **CI image install point.**
   - The AIDL HAL loads `libtinyalsav2.so` from
     `/apex/com.android.hardware.audio/lib64`. The symbol set matches (checked
     against the CI APEX), and the tone works there.
   - For that run, the library sat in the image lane's
     `session/image-overlay/cuttlefish/apex/...`. A built artifact needs a
     generated layer instead. Proposal: `audio/build.sh` installs into
     `out/apex-overlay/<apex>/`, and ns-init stacks it above `$apex` for the
     APEXes the image has. The image lane is reworking ns-init now, so this
     lands with them.
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
