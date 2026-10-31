# bridge-client

The C client of the [bridge protocol](../../docs/bridge-protocol.md) for guest
code, Android or OpenHarmony, mirroring
`host/bridge/src/ipc/{wire,transport,proto}.rs`: connect to
`/dev/socket/innkeeper/bridge.sock`, say HELLO, build messages, send and receive
them with fds, and match replies by tag while skipping events.

A static library in the NDK superbuild (`innkeeper_bridge_client`), linked by
Android's composer (`BridgePresenter`, display service), the Health HAL (host
state service) and `libtinyalsav2` (audio service); it installs nothing. It uses
only POSIX, so OpenHarmony's composer compiles `bridge_client.c` into its musl
library ([ohos/vendor/display](../../ohos/vendor/display/README.md)).

- `include/innkeeper/bridge_client.h`: framing, connecting and reconnect
  backoff.
- `include/innkeeper/audio.h`: the audio service's shared connection, PCM
  open/close/standby, ring reads and writes, presentation timing and
  `lxb_set_metadata` (`SET_METADATA`: Android `audio_usage_t` and a label, so
  the desktop mixer can name a playback stream).

Tests run in `mise run test` (the `host-tests` preset): `tests/framing_test.c`
checks the encoding against the protocol and fd passing over a socket pair;
`tests/audio_test.c` checks capture underruns, partial reads and bridge death.
