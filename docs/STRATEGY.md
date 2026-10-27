# Linux.apk: Android apps on desktop Linux (strategy, draft v1)

Working name: Linux.apk (pending a courtesy heads-up to Mac.apk's author).
License: AGPL-3.0. Status: draft for alignment, 2026-10-26.

## Goal

Double-click an APK on desktop Linux and get a native-feeling app: its own
Wayland window, desktop dark mode, notifications, file pickers, media controls,
and no root at runtime. No VM, no visible Android "device", no
launcher-in-a-box. Banking and other apps that need Play Integrity are out of
scope.

## Landscape (researched 2026-10-26)

- **Waydroid** runs a full LineageOS in a root LXC container on binderfs. It's
  the most compatible option. It's heavy (full boot, a resident system image),
  and the desktop integration is weak. It isn't on Flathub, for the same reasons
  listed under Packaging below.
- **ATL** reimplements the framework on real ART 10 with GTK4. It's driven by
  two core developers adding stubs: about 18% of its own methods are empty. Over
  time it keeps replacing its own code with copied AOSP code.
- **Mac.apk** (macOS) reimplements the framework on HotSpot and reached 58 of 71
  tested apps working in about five months. It had to reimplement, because macOS
  has no Linux kernel, Bionic or ART.
- **Nucleus** (MIT) is a Compose Desktop host. It isn't an Android layer, but
  it's the best reference for Linux desktop integration: portals, MPRIS,
  notifications, AT-SPI through AccessKit, clipboard, and drag and drop.

## Threat model (decided 2026-10-27)

**Desktop parity:** an Android app may do what a native desktop app running as
the user can do. Linux.apk adds no protection that costs complexity, hacks or
performance beyond that.

- **Keep:** isolation that comes for free from the namespaces the session needs
  anyway, and bounds checking that is simple and serves correctness (sizes,
  overflow, never writing into the wrong process).
- **Out of scope:** making host helpers robust against deliberately malicious
  Android processes (session DoS, kernel-exact fault semantics for malformed
  requests that only hurt the caller).
- **Outer boundary:** the packaging (Flatpak) provides it, as it does for any
  other desktop app.

## Non-goals

- **Virtual machines.** Linux.apk runs Android natively, as host processes
  integrated with the desktop. VM-based Android already exists (the emulator,
  Cuttlefish, Waydroid-like stacks, ChromeOS ARCVM); a VM or guest kernel is
  never an acceptable architecture here.

## Core bet: real Android userspace, our own bottom layer and UX

Android _is_ Linux. We run a real, near-stock AOSP x86_64 userspace (ART,
framework, system_server, SurfaceFlinger) in a rootless, on-demand session, and
put our work at Android's **stable boundaries**:

- **Vendor HALs** (Treble's stable interfaces): a hwcomposer that turns each
  layer or task into a Wayland surface, gralloc backed by GBM, audio through
  PipeWire, input from the Wayland seat. Waydroid's HALs (Apache-2.0) are a
  starting point.
- **Mesa built for Bionic** for GLES and Vulkan. Waydroid proves this works on
  AMD (radeonsi, RADV).
- **Our own privileged system app/service plus overlays** for policy:
  `uiMode`/density from desktop settings, handlers for document-picker and share
  intents, notification and MediaSession listeners.
- **A glibc "desktop bridge" helper** reached over a Unix socket. It talks to
  D-Bus portals (FileChooser, OpenURI, Settings), notifications, MPRIS, AT-SPI
  and the tray. Nucleus patterns are the reference.
- **A host launcher:** installs an APK into the session, generates `.desktop`
  files and icons, and opens apps as individual windows.

Compatibility comes from AOSP's own code. Everything we write sits at interfaces
designed to be implemented out of tree, so routine work needs no AOSP rebuilds.

```
 host (glibc)                         session: user+mount namespace, subuid map
 ┌─────────────────────┐  socket   ┌──────────────────────────────────────────┐
 │ linuxapk launcher   │◄─────────►│ init (minimal rc) · servicemanager       │
 │ desktop bridge:     │           │ system_server (trimmed via config)       │
 │  portals, notifs,   │           │ surfaceflinger ─► our HWC HAL ─► Wayland │
 │  MPRIS, AT-SPI      │           │ zygote ─► app processes (one uid each)   │
 └─────────────────────┘           │ HALs: gralloc/GBM, audio/PipeWire, input │
        Wayland compositor ◄───────│ Mesa (Bionic) on /dev/dri/renderD*       │
                                   │ binderfs (private instance)              │
                                   └──────────────────────────────────────────┘
```

**How this differs from Waydroid:**

- rootless, using user namespaces instead of a root LXC daemon;
- started on demand and stopped when idle, with no visible boot;
- per-app windows by default, plus APK double-click install and `.desktop`
  integration;
- desktop integration through portals;
- the current Android release (17).

**Not doing:** a fake system_server, or patching libgui to drop SurfaceFlinger.
Both depend on Android internals that aren't stable. We keep them as a later
memory optimisation only if the measured session cost justifies it.

**Pin: Android 17** (`android-17.0.0_r1`, the latest public source).

## Build strategy: avoid AOSP builds on the hot path

- **Prototyping:** Google's prebuilt AOSP emulator image
  (`system-images;android-37.0;default;x86_64`, 900 MB). We take its system,
  system_ext and product partitions and its APEXes, and supply our own vendor
  side. This is dev-only: it's under the Android SDK license, so we can't
  redistribute it.
- **Distribution image:** built from AOSP source by one dedicated image-builder
  lane on this machine. It rebuilds only when the pin moves or our small AOSP
  patch queue changes. The checkout uses `repo sync -c --depth=1` plus ccache:
  roughly 100 GB of source and 200–300 GB of `out/`. A full build takes a few
  hours on 32 threads; a module rebuild takes minutes.
- **Everything else builds out of tree** in our normal git repo against a
  "platform sysroot" (headers, stub libs, NDK/VNDK) exported from that build:
  HALs, Mesa, the session tooling, the bridge, and our system app. Builds take
  seconds to minutes, and agents work in ordinary git worktrees in parallel.
- **AOSP patches are a last resort.** Each one is a file in our repo's
  `aosp-patches/` and needs a written reason why no stable boundary works.

## Root and the binder transport

**Dev machines need root once, not during development.** Fedora's SELinux denies
`binder { set_context_mgr }` to ordinary user processes (verified: AVC denial
for `unconfined_t`). A 10-line dev policy module
([`selinux/linuxapk_dev.te`](../selinux/linuxapk_dev.te), compiles cleanly)
grants binder permissions to `unconfined_t` on the user's own private binderfs
instances. The user installs it once with `sudo semodule -i linuxapk_dev.pp`.
After that:

- nothing per build or per test needs root, because the policy keys on the
  process domain rather than a labelled binary path;
- user namespaces, binderfs mounts, bind and overlay mounts, `/dev/dri` and the
  subuid mapping (`sargunv:524288:65536`) are all unprivileged;
- build dependencies live in rootless podman containers, so agents never need
  `sudo`.

**Replacing kernel binder** with a userspace transport, "ubinder", is the key to
running with no privileges anywhere:

- **Approach:** an `LD_PRELOAD` shim for Bionic processes intercepts `open`,
  `ioctl(BINDER_*)` and `mmap` on binder device paths. It forwards the binder
  command stream to a per-session broker daemon over Unix sockets, passing fds
  with `SCM_RIGHTS`. Incoming transaction buffers land in a shared memfd that
  stands in for the kernel's mmap area. There are no AOSP patches, and it covers
  libbinder, libhwbinder and Rust binder at once, on the prebuilt image too.
- **What the broker reimplements:** the kernel driver's semantics. That means
  per-process handle tables, strong and weak refcounts, translating binder
  objects and fds inside transactions, routing nested sync calls back to the
  waiting thread, oneway queues per node, thread-pool spawn requests, death
  notifications, and the sender pid/uid (which the broker can _virtualize_).
- **Size:** about the kernel's `binder.c` (~7k lines of C).
- **Oracle:** AOSP's own binder test suites (`binderLibTest`,
  `binderDriverInterfaceTest`, the throughput benchmarks) run against both
  kernel binder and ubinder for differential testing.
- **Cost:** an extra copy and hop through the broker. Measure it. Input events
  use sockets, not binder.

## Languages

Pick the language by where the code runs:

- **C/C++ inside Android processes and on C ABIs:** the vendor HAL services (the
  AIDL NDK backend generates C++), mapper and Vulkan `.so` files loaded into
  every process, `LD_PRELOAD` shims, and Mesa patches. Keep these small.
- **Rust for new standalone components,** especially at security boundaries:
  - the ubinder broker (parses untrusted input from every Android process);
  - the session launcher (namespace and mount setup; bash today, rewritten once
    its design settles);
  - the desktop bridge (zbus, ashpd, smithay-client-toolkit, AccessKit);
  - the host launcher and packaging tools.
- **Not Zig:** it has no binder/AIDL ecosystem, it's still pre-1.0, and the NDK
  already covers cross-compiling C.

## Packaging and distros

**Scope (user decision, 2026-10-27):**

- Linux.apk _itself_ ships as a **Flatpak** (and, if practical, an AppImage).
  Users install it like Gear Lever, then install APKs through it as normal Linux
  apps.
- **Installing anything must never require `sudo`:** no RPM/deb/AUR system
  packages, SELinux modules, AppArmor profiles or kernel modules.
- Packaging individual Android apps as their own Flatpaks is a possible extra,
  **not** a design constraint.

What that requires of the runtime:

| Need                     | How                                                                                                         |
| ------------------------ | ----------------------------------------------------------------------------------------------------------- |
| IPC without root         | ubinder (userspace binder; no binderfs, no SELinux module)                                                  |
| uids without host setup  | automatic: the subuid range when the host has one, single-uid (virtual uids) otherwise, e.g. inside Flatpak |
| `/system`, `/apex` paths | bind mounts in a user namespace natively; inside Flatpak, a symlink root (`docs/sandbox.md`)                |

Distro differences that don't need root:

|                              | Fedora/openSUSE | Ubuntu 24.04+                                | Debian  | Arch    | Flatpak                                 |
| ---------------------------- | --------------- | -------------------------------------------- | ------- | ------- | --------------------------------------- |
| unprivileged user namespaces | yes             | restricted by AppArmor (but Flatpak is fine) | yes     | yes     | no nested namespaces: symlink-root mode |
| binder                       | ubinder         | ubinder                                      | ubinder | ubinder | ubinder                                 |

## Milestones

|                            | Exit criterion                                                                                                                                                                                                                                                                                                                                                              |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **M0 Session boots**       | From the prebuilt emulator image: a rootless namespace, private binderfs, minimal init, servicemanager, system_server and zygote reach "boot completed" on this machine. In parallel, the image-builder lane produces our own source build.                                                                                                                                 |
| **M1 Pixels**              | An APK's Activity renders through SurfaceFlinger and our HWC into a Wayland window (start with one surface for the whole display, then per task). Pointer, keyboard and scroll work. First noarch apps are usable: OpenCalc, Open Sudoku, Unit Converter, Now in Android.                                                                                                   |
| **M2 Real apps**           | Bionic Mesa for the GPU, PipeWire audio, per-app windows (resize, close, back), the launcher and `.desktop` install. Games with x86_64 libs: Mindustry, Shattered Pixel Dungeon, OpenTTD, Unciv. _ubinder starts in parallel, differential-tested against kernel binder._                                                                                                   |
| **M3 Desktop integration** | The bridge: dark mode and accent, fractional scaling to density, notifications, file picker, share and open URI, MPRIS, clipboard, IME through `text-input-v3`. Cross-app intents, F-Droid installing apps, microG.                                                                                                                                                         |
| **M4 No-privilege mode**   | ubinder becomes the default (no binderfs, no SELinux module), virtual uids (`--single-uid`, no subuid range needed), the image switched to Google's AOSP CI build (downloaded at setup instead of built or redistributed), and corpus parity in every mode. Relocating paths was assessed (`docs/sandbox.md`) and moved to M5, where Flatpak needs it. **Done 2026-10-27.** |
| **M5 Packaging**           | Ship Linux.apk itself as a Flatpak (AppImage if practical), installable with no `sudo` (Gear Lever model): the session runs inside the Flatpak per `docs/sandbox.md` (symlink root, automatic single-uid, ubinder). Idle shutdown, startup and memory budgets. aarch64 hosts. Per-app Flatpaks are an optional extra.                                                       |
| **M6 ARM translation**     | arm64-only libs on x86_64 hosts through a NativeBridge (QEMU-user, as in ATL's nb-qemu, or a faster open translator). Wanted before launch if practical.                                                                                                                                                                                                                    |
| **Later**                  | Accessibility through AT-SPI.                                                                                                                                                                                                                                                                                                                                               |

## ABI survey (2026-10-26)

We sampled 47 popular apps (APKMirror variant listings). Every app in the sample
contains native code.

- **Non-game apps:** 20 of 26 (~77%) ship x86_64. Google, Meta, Microsoft,
  browsers, messaging and media apps all do. Flutter and React Native include
  x86_64 by default. Gaps: TikTok, CapCut, Snapchat, Amazon, Temu, Edge.
- **Top games:** 3 of 21 (~14%) ship x86_64. Unity never enabled it by default
  and removed it entirely in 6.5. Unreal ships arm64 only unless you build the
  engine from source.
- **Apps with no native code at all:** published studies put this at roughly
  40–60% of apps.
- **Conclusion:** without translation, x86_64 covers most everyday apps and
  almost no popular games. Every commercial x86 Android platform (ChromeOS, WSA,
  Play Games on PC, Googlebooks) relies on proprietary translation.

Test-corpus implication: through M4, pick game targets that ship x86_64 or are
FOSS (Mindustry, Shattered Pixel Dungeon, OpenTTD, Unciv, Clash Royale, Roblox).

## Testing and the agent loop

- **Corpus:** F-Droid APKs (redistributable, so they can be cached in CI), plus
  Mac.apk's compatibility list as targets. Non-free APKs stay local only.
- **Smoke harness:** install, launch into a headless nested compositor (headless
  KWin or `weston --backend=headless`), then check for crash or ANR, a first
  frame, a non-blank screenshot, and a scripted interaction. Output is a
  checked-in `COMPATIBILITY.md`.
- **Gate:** no change lands if it regresses an app that passed before.
- **Budgets tracked in the harness:** cold session start, warm app start,
  session RSS. These keep us honest about "lighter than Waydroid".

## Agent workstreams

The orchestrator sequences these, reviews diffs, runs the gate and keeps this
doc current.

1. **Image builder** (serial, owns the AOSP tree): checkout, the patch queue,
   builds, sysroot export.
2. **Session** (M0): the namespace launcher, binderfs, the property area,
   minimal init rc, subuid mapping.
3. **Display** (M1–M2): the HWC-to-Wayland HAL, gralloc on GBM, Bionic Mesa.
4. **Input and windowing** (M1–M3): seat to input, per-task windows, insets,
   scaling.
5. **Audio and media** (M2): the PipeWire audio HAL, codecs.
6. **Desktop bridge and launcher** (M2–M3): the glibc helper, `.desktop` files,
   portals.
7. **Compat harness** (from M1 on): the corpus, the runner, triage, the compat
   table.
8. **ubinder and no-privilege mode** (M2–M4): the broker, the LD_PRELOAD shim,
   virtual uids, path relocation.

## Top risks

1. **Booting without a full Android init** (M0). It may need more of init's
   environment than expected (properties, mounts, apexd). Fallback: run the real
   init with a trimmed rc inside the namespace, which is still rootless.
2. **Per-app windows through SurfaceFlinger plus HWC.** Mapping tasks and layers
   to Wayland toplevels and popups is the core UX work. Waydroid's multi-window
   mode is prior art.
3. **Session weight.** A real system_server costs hundreds of MB. Mitigations:
   disable services by config, start on demand, stop when idle. Measure before
   doing anything more invasive.
4. **ubinder fidelity and performance.** It reimplements subtle kernel
   semantics. AOSP's binder tests serve as a differential oracle, and kernel
   mode stays as the reference.
5. **Prebuilt image vs source image drift.** We switch to our own build by M4 at
   the latest, and keep HAL code dependent only on stable interfaces.
