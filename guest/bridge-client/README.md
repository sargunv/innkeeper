# bridge-client

The C client of the [bridge protocol](../../docs/bridge-protocol.md) for guest
code, Android or OpenHarmony, mirroring
`host/bridge/src/ipc/{wire,transport,proto}.rs`: connect to
`/dev/socket/innkeeper/bridge.sock`, say HELLO, build messages, send and receive
them with fds, and match replies by tag while skipping events.

A static library in the NDK superbuild (`innkeeper_bridge_client`), linked by
Android's composer (`BridgePresenter`, display service), the Health HAL (host
state service) and the audio HAL (audio service, whose client lives in
[android/vendor/audio](../../android/vendor/audio/README.md)); it installs
nothing. It uses only POSIX, so OpenHarmony's composer compiles
`bridge_client.c` into its musl library
([ohos/vendor/display](../../ohos/vendor/display/README.md)).

`include/innkeeper/bridge_client.h` has the framing, connecting and reconnect
backoff.

Tests run in `mise run test` (the `host-tests` preset): `tests/framing_test.c`
checks the encoding against the protocol and fd passing over a socket pair.
