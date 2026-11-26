# camera

This directory holds the camera HAL,
`android.hardware.camera.provider.ICameraProvider/innkeeper/0` (AIDL V1, device
V1). It gives Android the desktop's camera through the bridge's camera service
([camera/mod.rs](../../../host/bridge/src/camera/mod.rs)), which streams it from
the XDG Camera portal's PipeWire connection. `mise run build:ndk` builds it out
of tree with the NDK and stages it as
`/vendor/bin/hw/android.hardware.camera.provider-service.innkeeper`, with its
init rc and VINTF fragment. [image/bootconfig](../../../image/bootconfig) drops
cuttlefish's emulated camera APEX.

- **Device.** The provider asks the bridge at start whether the desktop has a
  camera (`IsCameraPresent`) and lists `device@1.1/innkeeper/0` if so. Hotplug
  isn't reported. The device looks like AOSP's external (USB) camera: hardware
  level EXTERNAL, facing EXTERNAL, fixed focus, automatic exposure and white
  balance, 15 to 30 fps. It offers 1280x720, 640x480, 320x240 and 176x144 as
  `IMPLEMENTATION_DEFINED`, `YCbCr_420_888` and JPEG.
- **Consent.** cameraserver opens the device only for an app that holds
  `CAMERA`. The session then asks the bridge to stream, and the bridge asks the
  portal on first use. A portal denial makes the open session report
  `ERROR_DEVICE`, and later sessions fail to configure, for the rest of the
  bridge's lifetime.
- **Frames.** The bridge streams near the largest configured size and writes
  NV12 into a read-only frame ring. For each request, the session takes the
  newest frame, crops its center to each stream's aspect ratio and scales it
  with nearest neighbour into the gralloc buffer, which it maps through our
  allocator's handle (`graphics/common`). JPEG streams get a baseline JFIF
  encoded here (`Convert.cpp`). Until the first frame arrives, for example while
  the portal's dialog is open, frames are black at 30 fps, so cameraserver's
  pipeline doesn't stall.
- **Metadata** travels inline, without FMQs. Results repeat the request's
  settings and add the timestamp and converged 3A states.
- **LL-NDK only.** The HAL links libbinder_ndk and libcamera_metadata through
  link-only stubs of their AOSP symbol maps, and the frozen camera AIDL
  snapshots come from `android/vendor/third_party/aosp`.

Tests: `tests/convert_test.cpp` runs in `mise run test` and checks scaling into
each YCbCr layout and the JPEG stream. The camera group of `mise run test:e2e`
plays a white virtual camera on the nested desktop's PipeWire. The fake Camera
portal allows it, and a Camera2 ImageReader in the probes app must see white
frames. Start reading at `src/Session.cpp`.
