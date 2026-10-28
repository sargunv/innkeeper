# bridge-client

The C client of the [bridge protocol](../../../docs/bridge-protocol.md) for code
inside Android, the mirror of `host/bridge/src/ipc/{wire,transport,proto}.rs`:
connect to `/dev/socket/linuxapk/bridge.sock`, say HELLO, build messages, send
and receive them with fds, and match replies by tag while skipping events.

A static library in the NDK superbuild (`linuxapk_bridge_client`), linked by the
composer's `BridgePresenter` (display service), the Health HAL (host state
service) and `libtinyalsav2` (audio service); it installs nothing. API:
`include/linuxapk/bridge_client.h`. `tests/framing_test.c` runs in
`mise run test` (the `host-tests` preset): it checks the encoding against the
protocol and fd passing over a socket pair.
