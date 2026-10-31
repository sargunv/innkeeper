# bridge-client

The C client of the [bridge protocol](../../docs/bridge-protocol.md) for code
inside a guest, Android or OpenHarmony, the mirror of
`host/bridge/src/ipc/{wire,transport,proto}.rs`: connect to
`/dev/socket/innkeeper/bridge.sock`, say HELLO, build messages, send and receive
them with fds, and match replies by tag while skipping events.

A static library in the NDK superbuild (`innkeeper_bridge_client`), linked by
Android's composer (`BridgePresenter`, display service), the Health HAL (host
state service) and `libtinyalsav2` (audio service); it installs nothing. It uses
only POSIX, so OpenHarmony's composer compiles `bridge_client.c` into its musl
library ([ohos/vendor/display](../../ohos/vendor/display/README.md)). API:
`include/innkeeper/bridge_client.h` for framing, `include/innkeeper/audio.h` for
PCM streams. `audio.c` owns the shared audio connection, open/close/standby,
ring reads/writes and presentation timing, with no tinyalsa types.
`tests/framing_test.c` runs in `mise run test` (the `host-tests` preset): it
checks the encoding against the protocol and fd passing over a socket pair.

`tests/audio_test.c` checks capture underruns, partial reads and bridge death.
`lxb_set_metadata` sends `SET_METADATA` (stream, Android `audio_usage_t`, label)
so the desktop mixer can name a playback stream by what is playing.
