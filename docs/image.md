# The Android image

Linux.apk boots an unmodified AOSP build from **Android's public CI**
(ci.android.com). Each user downloads it from Google at setup, and we never
redistribute it. It replaces the SDK emulator image
(`system-images;android-37.0;default;x86_64`, under the Android SDK license),
which is still selectable for comparison.

## The pin

[`image/pin.toml`](../image/pin.toml):

| Field    | Value                                                                                  |
| -------- | -------------------------------------------------------------------------------------- |
| Branch   | `aosp-android-latest-release`                                                          |
| Build    | `16644040`                                                                             |
| Target   | `aosp_cf_x86_64_only_phone-userdebug` (cuttlefish, x86_64 only)                        |
| Release  | Android 17, SDK 37, `CP2A.260605.016`, test-keys (the same release as `~/aosp/a17`)    |
| Artifact | `aosp_cf_x86_64_only_phone-img-16644040.zip`, 1,163,636,385 bytes, SHA-256 `eccdb248…` |
| Artifact | `sbom/sbom.spdx.json`, 26,142,886 bytes, SHA-256 `5f282fcb…`                           |

## Setup

```sh
tools/fetch-image.sh                  # download, verify, extract (about 10 s on a fast link)
session/linuxapk-session start        # LINUXAPK_IMAGE defaults to the pin
LINUXAPK_IMAGE=emulator session/linuxapk-session start   # the SDK image in out/img
```

`linuxapk-session start` runs `fetch-image.sh` itself if the pinned image is
missing. Files live in `~/.local/share/linuxapk/images/<name>/` (override with
`LINUXAPK_IMAGES`), shared by every checkout and instance:

- `dl/` holds the artifacts. Downloads resume (`curl -C -`; the CI serves GET
  only, HEAD is 404), and each file is checked against the pinned size and
  SHA-256. A mismatch deletes the file and fails.
- `img/` holds the partitions in the layout `out/img` has:
  - `system` (system-as-root), `system_ext`, `product`, `vendor`, `odm`,
    `system_dlkm`, `vendor_dlkm` and `odm_dlkm`, from the sparse `super.img`
    (`simg2img`, `lpunpack`, slot `_a`, all erofs);
  - `ramdisk`, from `init_boot.img`.
  - `tools/extract-image.sh` takes either the SDK image directory or this zip.
- `apex-<hash>/` holds the APEXes, flattened by `tools/flatten-apex.sh`, one
  tree per vendor APEX selection (the hash covers the selection).
  - A tree is never rewritten under a running session.
  - Delete stale ones by hand.
- Total size: about 1.2 GB downloaded, 2.4 GB extracted, and 0.75 GB per APEX
  tree.

Every image partition is mounted read-only, with our layers on top.
`session/ns-init.sh` stacks, topmost first:

1. `session/image-overlay/<kind>`: per image kind (`cuttlefish` here);
2. `session/rootfs-overlay`;
3. `out/vendor-overlay` and `out/system_ext-overlay`;
4. the generated `out/session/overlay`;
5. the image.

## What the CI image needs (`session/image-overlay/cuttlefish/`)

### Vendor APEX selection

**Vendor APEX selection** (`cmdline`) uses cuttlefish's own mechanism,
`androidboot.vendor.apex.<apex name>=<file>|none` (`ro.boot.vendor.apex.*`).
apexd activates only the selected file, and `none` drops the APEX together with
its services, VINTF fragments and feature files. `flatten-apex.sh` applies the
same rule, and apexd must agree with it: a mismatch aborts apexd in
`getActivePackages`, which reboots the session.

| APEX                                                                                                                                                                                              | Choice                                                                                      |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `com.android.hardware.keymint`                                                                                                                                                                    | `…keymint.rust_nonsecure` (not `rust_cf_remote` over vsock, or the Trusty VM variant)       |
| `com.android.hardware.gatekeeper`                                                                                                                                                                 | `…gatekeeper.nonsecure` (not `cf_remote`)                                                   |
| `com.android.hardware.graphics.composer` (ranchu, drm_hwcomposer), `com.google.cf.gralloc` (minigbm), `com.google.cf.vulkan` (gfxstream)                                                          | `none`: our allocator, mapper, composer3 and RADV/lavapipe win                              |
| `com.google.cf.{bt,rild,wifi,wpa_supplicant,nfc,oemlock,identity,confirmationui,light}`, `com.android.hardware.{wifi,gnss,uwb,usb,boot,threadnetwork}`, `com.google.emulated.camera.provider.hal` | `none`: virtio consoles, vsock to cuttlefish host services, the modem simulator, VM devices |
| `com.google.cf.disabled`                                                                                                                                                                          | `none` (the image's own `init_dev_config` disables it too)                                  |

These stay active: the AIDL audio HAL, health (cuttlefish's fake battery),
power, thermal, vibrator, memtrack, authsecret, authgraph, secretkeeper, CAS,
ClearKey DRM, contexthub, dumpstate, IR, input processor, NNAPI samples,
nlinterceptor, rebootescrow, secure element, tetheroffload and
`cf.input.config`. The emulator image had a subset of these.

### Vendor properties

`vendor.prop` is merged into the generated `/vendor/build.prop`. Keys set by the
session replace the image's own values.

- `ro.product.board=linuxapk` (was `cutf`). Cuttlefish userdebug builds turn on
  BPF map lock checks (OFD locks on map fds, `lockingEnabled` in
  `BpfSyscallWrappers.h`). netd's `mapLockTest` aborts on them, because the
  session's emulated maps (`helper/bpfemu.c`) give each `BPF_OBJ_GET` its own
  file. This was only a dev correctness check. The alternative is to make bpfemu
  reopen one inode per map.
- `ro.frp.pst=`: there's no block device, so PersistentDataBlockService isn't
  started (with the property set, it failed on `/dev/block/by-name/frp`). Same
  for `ro.rebootescrow.device`.
- `ro.lockscreen.disable.default=true`: no lock screen, as on the emulator
  image.
- `ro.product.cpu.abilist=x86_64`, and `abilist64` likewise. Cuttlefish lists
  `arm64-v8a` for a native bridge it doesn't have here
  (`ro.dalvik.vm.native.bridge=0`), so arm64-only apps installed and then failed
  at runtime. Setting the values explicitly stops init from deriving them from
  `ro.odm.product.cpu.abilist`.

### Init additions

`vendor/etc/init/linuxapk-cuttlefish.rc` runs once boot completes and sets
`settings put secure immersive_mode_confirmations confirmed`. Without it,
SystemUI's "Viewing full screen" cling takes focus from every immersive app
(Shattered Pixel Dungeon, Unciv and OpenTTD regressed). The emulator image never
shows it; the likely difference is its desktop-mode resources (see "Open
issues").

### Blanked files

These follow the existing overlay pattern:

- `vendor/etc/init/init.cutf_cvm.rc`: the device rc. It loads VM kernel modules,
  runs `mount_all`, proxies adb over vsock, and starts Wi-Fi/modem setup. With
  `androidboot.hardware=linuxapk`, `init.linuxapk.rc` does what's needed
  instead.
- `vendor/etc/init/seriallogging.rc`: `logcat -b all` to a virtio console.
- `system_ext/etc/init/rpmb_dev.test.system.rc`: an RPMB mock for Trusty test
  VMs.
- `product/etc/init/init.rc`: `tombstone_transmit` over vsock.
- VINTF `face-default.xml` and `fingerprint-default.xml`: HALs that are declared
  but never started.
- Empty APKs: `vendor/priv-app/CuttlefishService` and
  `system_ext/priv-app/CFSatelliteService`.
- An empty `vendor/overlay/cuttlefish_overlay_connectivity.apk`. That RRO's only
  resource is `config_ethernet_interfaces = eth1;11,12,14`, which leaves out
  NOT_RESTRICTED. It made pasta's `eth1` a restricted network: no default
  network, so apps were offline (Wikipedia and F-Droid regressed). Without it,
  `eth1` gets EthernetTracker's default capabilities, as on the emulator image.
- `vendor/etc/permissions/linuxapk-unavailable-hardware.xml` marks Bluetooth
  unavailable. `handheld_core_hardware.xml` declares it, and without a HCI HAL
  `com.android.bluetooth` aborted in a loop.

The common overlay's goldfish blanks (sensors multihal, `mapper.minigbm.xml` and
the rest) also apply to cuttlefish files with the same names.

### Session changes for any image

- `/cache` is a symlink to `/data/cache` in this image (there's no cache
  partition).
- `odm` is its own partition.
- `*_dlkm` are mounted when present.
- Overlay mounts use `lowerdir+` when the option string is longer than 256
  bytes. This is the same fix as the win lane's.

### Graphics, networking and init

Everything else carries over unchanged:

- graphics: our HALs via `out/vendor-overlay`, `ro.hardware.egl=angle`,
  `ro.hardware.vulkan=radv|lvp`;
- networking: netd with emulated eBPF, nf_tables iptables, EthernetService on
  pasta's `eth1`;
- init: the stock `init.rc` and the other system rc files we override are
  byte-identical in both images.

## Audio (interim, until the audio lane's HAL lands)

The CI vendor has no HIDL audio wrapper, only AOSP's default **AIDL** HAL
(`com.android.hardware.audio`). It works as the interim stub with one change.
Its stream worker threads call `setpriority(ANDROID_PRIORITY_AUDIO)` and fail
the stream open on `EACCES`, because there's no `CAP_SYS_NICE` over the initial
namespace. audioserver then blocks in `openOutput`, and with it system_server's
AudioService. The watchdog kills system_server every 65 s and boot never
completes.

The fix: `apex/com.android.hardware.audio/etc/*.rc` is the image's file plus
`setenv LD_PRELOAD /system/lib64/liblinuxapk_shim.so`. The shim makes priority
changes best-effort. `/system/lib64` is in that APEX's linker namespace and
`/vendor/lib64` is not. With no `/dev/snd` the primary module still opens
streams, and r_submix and bluetooth load.

The audio lane's plan (`docs/audio.md`, option (c)) replaces
`/apex/com.android.hardware.audio/lib64/libtinyalsav2.so`. It needs this rc
override too, so keep it, or move it into their layer, when they land.

## Comparison with the emulator image

Measured on the dev host (Strix Halo, RADV), own instance, fresh `/data`. Boot
is from `linuxapk-session start` to `sys.boot_completed=1`. Memory is the
session cgroup's `memory.current`, 60 s after boot completed.

| Image (headless, fresh `/data`)        | Boot to `boot_completed` | Idle `memory.current` | of which anon |
| -------------------------------------- | ------------------------ | --------------------- | ------------- |
| ci-16644040 (4 runs)                   | 23.6–24.2 s              | 764–893 MiB           | 516–524 MiB   |
| emulator (CE2A.260420.051.A3/16571990) | 22.5 s                   | 770 MiB               | 531 MiB       |
| ci-16644040 with the Wayland window    | 23.7 s                   | 645 MiB               | 359 MiB       |

Boot times match within about 1.5 s, and anonymous memory is the same. The
spread in `memory.current` is page cache (`file` ranged 39–167 MiB).

## Open issues

- **Desktop mode.** The emulator's vendor RRO sets
  `config_isDesktopModeSupported=true`; cuttlefish's doesn't. The windowing lane
  should check whether per-app windows depend on it. If they do, enable it
  through a developer setting or our own RRO.
- **BPF map lock checks** are off (`ro.product.board`). Making `bpfemu` reopen
  one inode per map would let them run.
- **Stale APEX trees** (`apex-<hash>`) aren't garbage-collected.
- **Image-only blanks:** the goldfish blanks in `session/rootfs-overlay` could
  move to an `image-overlay/emulator` layer once the emulator image is retired.

## Licensing

The image is an AOSP build. Its SPDX SBOM (`sbom.spdx.json`, SPDX 2.3, 713
packages, 8,741 files, 755 license texts) lists the following for the 3,918
installed files (a file can carry more than one):

| License family                                          | Files |
| ------------------------------------------------------- | ----: |
| Apache-2.0                                              | 2,728 |
| MIT / ISC                                               |   507 |
| NOASSERTION (see below)                                 |   225 |
| SIL OFL (fonts)                                         |   201 |
| public domain (sepolicy, libselinux, hyphenation, …)    |    99 |
| BSD                                                     |    80 |
| LGPL-2.1 (libnl, libexif, libsepol, liburing, …)        |    62 |
| zlib, Unicode/ICU                                       |    12 |
| GPL-2.0 (iptables, iproute2, e2fsprogs, dnsmasq tools…) |     3 |

Items to flag:

- **No installed file is under a non-open-source license, with these
  exceptions:**
  - `libdng_sdk.so` uses Adobe's DNG SDK license (permissive, redistributable,
    but not OSI);
  - `libultrahdr`, `libjpegencoder` and `libjpegdecoder` carry an Adobe gain-map
    attribution notice.
  - Both ship in every AOSP build.
- **"opensourcerequest"** (91 files, mostly mainline APEXes) is a notice that
  points to source.android.com for source. It isn't a license restriction.
- **NOASSERTION** covers:
  - build-generated files (aconfig flag libraries, rc and xml files);
  - the CA certificates;
  - the prebuilt WebView (`external/chromium-webview`, Chromium/BSD);
  - the cuttlefish kernel and dtb (GPL-2.0). These are in `boot.img`, which we
    don't extract.
- **The Android SDK license** (`LicenseRef-prebuilts-sdk-license`) appears only
  on 99 build-time inputs from `prebuilts/sdk` (AndroidX and support jars
  compiled into apps such as Launcher3 and SystemUI). No installed file carries
  it.
  - Those libraries are Apache-2.0 upstream.
  - This is the one place where the image's license metadata mentions non-open
    terms.

Distribution:

- Linux.apk never redistributes the image.
- `tools/fetch-image.sh` downloads it from Google's CI on the user's machine,
  verifies the pinned hashes, and extracts it locally.
- Nothing from the image is committed to this repo.

## Bumping the pin

1. Pick a build on ci.android.com: branch `aosp-android-latest-release`, target
   `aosp_cf_x86_64_only_phone-userdebug`. Ideally its `BUILD_ID` matches the
   AOSP tag we build against.
2. Download the artifacts and record their sizes and hashes:
   ```sh
   b=<build>; t=aosp_cf_x86_64_only_phone-userdebug
   u=https://ci.android.com/builds/submitted/$b/$t/latest/raw
   curl -fLO "$u/aosp_cf_x86_64_only_phone-img-$b.zip"; curl -fLO "$u/sbom/sbom.spdx.json"
   sha256sum *.zip sbom.spdx.json; stat -c '%s %n' *.zip sbom.spdx.json
   ```
3. Update `name`, `build_id`, `fingerprint` and the `[[artifact]]` entries in
   `image/pin.toml`. The default `LINUXAPK_IMAGE` follows `name`.
4. Run `tools/fetch-image.sh`, then check the new vendor APEX list
   (`ls ~/.local/share/linuxapk/images/<name>/img/vendor/apex`). Add a selection
   for every new multi-install or VM-only APEX in
   `session/image-overlay/cuttlefish/cmdline`.
5. Check `init.cutf_cvm.rc` and the system rc files we override
   (`session/rootfs-overlay/system/etc/init`) against the image's copies.
6. Prove it:
   - fresh `/data` to `sys.boot_completed=1`, `selftest` all ok;
   - `android/e2e/e2e.sh`;
   - `compat/run.sh` with no regressions.
7. Update this file's tables (SBOM summary, numbers) and `COMPATIBILITY.md`.
