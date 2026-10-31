# ohos/vendor/display

OpenHarmony's display HDI for the OH guest, at OH's vendor boundary: two VDI
libraries that take the place of the image's (which drive DRM/KMS and allocate
dumb buffers on `/dev/dri/card0`), installed in `/vendor/lib64` by
`ohos/sandbox.sh`'s overlay. The image's own `composer_host` and
`allocator_host` load them; no image file changes.

| File           | Library                                               | Loaded by                                     |
| -------------- | ----------------------------------------------------- | --------------------------------------------- |
| `composer.cpp` | `libdisplay_composer_vdi_impl.z.so`                   | `composer_host` (`dlsym` of each C function)  |
| `buffer.cpp`   | `passthrough/libdisplay_buffer_vdi_impl.z.so`         | `allocator_host`, and every process that maps |
| `hdi.h`        | OH's ABI types (BufferHandle, DisplayCapability, ...) |                                               |
| `log.h`        | hilog through `libhilog.so`'s C API                   |                                               |

- **Composer.** One display, 0, at 60 Hz: by default a 720x1280 phone panel at
  320 dpi (`INNKEEPER_OHOS_DISPLAY`, `<width>x<height>@<dpi>`, which
  `ohos/sandbox.sh` also gives DMS), reported as a built-in panel so it is OH's
  main screen. Every layer is composed by render_service (`COMPOSITION_CLIENT`);
  each committed client target goes to the bridge's display service
  ([docs/bridge-protocol.md](../../../docs/bridge-protocol.md)) as a LINEAR
  dma-buf, shown opaque. `DISPLAY_WINDOW` gives the display a desktop window of
  its own. Commit waits (up to two frames) for the compositor to release every
  other target, so render_service never draws into one being shown. A 60 Hz
  timer is the vsync. Without a bridge (`mise run ohos:test`) the display is
  headless.
- **Buffers.** Sealed memfds made into dma-bufs by `/dev/udmabuf`, as Android's
  CPU buffers are: LINEAR, 256-byte row pitch, mappable, importable by llvmpipe
  and the compositor. RGB 8888/565/888, 1010102 and YUV 4:2:0.
- **Build.** `ohos/build.sh`, with the NDK's clang for `x86_64-linux-musl`,
  musl's headers (a pinned release, headers only) and stand-ins that give the
  linker `libc.so`, `libc++.so` and `libhilog.so` sonames; the image's libraries
  resolve the symbols at load time. The C++ never includes libc++ headers: the
  few `std::vector` and `std::string` fields in the ABI are written in place
  (libc++'s `std::__h` layout), and the buffer VDI's vtable comes from declaring
  OH's interface in its order. The composer links the shared
  [bridge client](../../../guest/bridge-client/README.md).

render_service draws with Mesa's llvmpipe: the image's Mesa has no radeonsi and
CI has no GPU ([ohos/README.md](../../README.md#display)).

## One window, not one per app

OH composes every window into one screen buffer before the HDI sees it. Layers
reach the composer separately only when render_service offloads them to hardware
composition, which it does for self-drawing surfaces (video, XComponent), not
ordinary ArkUI windows. So per-app desktop windows can't come from this
boundary: the image runs SceneBoard (`/system/etc/sceneboard.config`), whose
window manager puts apps on screen 0, and OH's floating and freeform modes are
SceneBoard's too. Per-app windows would need one OH screen per app window (a
virtual display each, as Android's system app does with HWC virtual displays)
plus a window policy on the OH side that places each app on its own screen. For
now the bridge shows screen 0 as one phone-frame window.
