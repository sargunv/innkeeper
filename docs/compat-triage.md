# Compat harness and triage

The harness runs the F-Droid corpus in a live session and records whether each
app works. Results are in [COMPATIBILITY.md](../COMPATIBILITY.md) and
`compat/results.json`. This file lists what fails and why, ranked, for lane
assignment.

## Harness

```sh
compat/run.sh                    # whole corpus; headless session if none runs
compat/run.sh --only org.wikipedia,de.sudoq
compat/run.sh --window           # start the session with its window, to watch
compat/run.sh fetch              # download and verify the corpus only
compat/run.sh report             # rewrite COMPATIBILITY.md from results.json
```

Run it as a capped job on the lane's own session instance:
`systemd-run --user --unit=compat-run --collect -p MemoryMax=4G -E LINUXAPK_INSTANCE=compat -- compat/run.sh --no-finish`.

- **Corpus:** `compat/corpus.toml` holds pinned F-Droid APKs (version code, URL
  and SHA-256). They download into `out/corpus/` and are never committed. Each
  entry also records the app's framework, checked against the APK's contents
  (see "Frameworks" below), and optional `manual` notes for checks the harness
  doesn't do.
- **Per app** (`compat/src/app.rs`):
  1. Copy the APK into `/data/local/tmp` and run `pm install -r -g`.
  2. Resolve the launcher activity (or a TV app's leanback one), then launch it.
     In per-app window mode (`vendor.linuxapk.wm.mode`, the default) it opens in
     its own window and display. A system prompt the app opens first, such as
     the battery-optimisation request, also counts as the first frame.
  3. For 8 s (15 s for games), poll the crash buffer, ActivityManager ANRs and
     `pidof`.
  4. Check that the app's window has focus. A platform "built for an older
     version of Android" dialog or the battery-optimisation prompt is noted and
     answered.
  5. `screencap`. Below the status bar, the frame is blank if one colour covers
     more than 99.5% of it. Apps with `FLAG_SECURE` (Ente Auth) are left out of
     screenshots by SurfaceFlinger, so for them the UI tree must have labelled
     nodes instead.
  6. Run the optional per-app script (syntax at the top of `corpus.toml`):
     - `tap`, `try`, `expect` and `absent` act on `uiautomator dump` nodes. A
       dump waits for the UI to go idle and may take long under CPU load (other
       sessions running): each try gets 150 s, with up to four tries, and their
       times go to `out/compat/<pkg>/ui-dump-times.txt`. **"uiautomator dump
       failed: Killed (after 0 s)"** was not a timeout: commands run through
       `linuxapk-session exec` inherit the caller's niceness, and uiautomator's
       handler thread, which sets its priority back to 0, dies with
       `SecurityException: Cannot set priority to 0` (crash buffer). zsh runs
       background jobs (`cmd &`) at nice 5. `compat/run.sh` now restarts itself
       at nice 0 through `systemd-run --user` when it finds itself niced;
     - `sh` runs a check in the session;
     - `bridgelog` checks that the host bridge logged a text since the script
       started (compat sessions only log links, docs/bridge.md);
     - `intent` opens a URI in the app;
     - `gesture` does a pinch, pan or double tap: real multi-touch through
       Linux.apk System's input bridge (`compat/gesture.sh`, protocol v2 in a
       per-app window);
     - `shot` and `changed` take screenshots that must not be blank, and
       `changed` must differ from the previous one.
  7. Tap the centre, scroll and press Back. After each one, check the app is
     still alive.
  8. Scan the log for known failure signatures: MediaCodec start errors, missing
     WebView, GL/EGL/Vulkan errors (`GL_INVALID_*`, `EGL_BAD_*`, MapLibre's
     `[OpenGL]`, `VK_ERROR_*`) and Google Play services.
  9. Save bounded logs and screenshots into `out/compat/<pkg>/`, then uninstall.
- **Result:** "works" means everything passed. "Partly works" means the first
  frame was fine but a later check failed. "Doesn't work" covers install,
  launch, early death, a blank frame or a dialog in front. Session restarts
  during an app are retried once and never count against the app.
- **Gate:** any app whose result is worse than in `compat/results.json` fails
  the run (exit 1), and the baseline is left alone. `--accept` records it
  anyway.
- **Session:** the harness uses this checkout's `session/linuxapk-session` if
  `out/img` exists (lane worktrees symlink `img`, `root`, `vendor-overlay` and
  `system_ext-overlay` from the main checkout's `out/`), else the main
  checkout's. Set `LINUXAPK_INSTANCE` to use a session of your own. If no
  session is running, it starts a headless one and waits for
  `sys.boot_completed`. At the end it leaves a session showing its window
  (`--no-finish` skips that).

## Results (2026-10-27)

On the default image, the AOSP CI build 16644040 (cuttlefish x86_64), 29 of 31
apps work in per-app windows. The two failures:

- **Open Sudoku:** its F-Droid build is arm64-only.
- **NewPipe:** works since the YV12 fix (issue 1 below); it used to crash
  SurfaceFlinger when playing a video.

The working apps install, render, survive input, reach the network and decode
audio. On the SDK emulator image, 30 of 31 worked: NewPipe's tap didn't start a
video there.

Screenshots: see the README (images are hosted as attachments on issue #1, not
committed).

### Frameworks

Each framework is checked from the APK's contents.

| Framework              | App (corpus)                                       | Evidence in the APK                                             | Result                               |
| ---------------------- | -------------------------------------------------- | --------------------------------------------------------------- | ------------------------------------ |
| Flutter                | Saber 1.36.3, Ente Auth 4.4.26                     | `lib/x86_64/libflutter.so`, `assets/flutter_assets/`            | Works                                |
| React Native           | Joplin 3.7.8                                       | `libhermes.so`, `libreactnative*`, Expo modules                 | Works (note opens in its WebView)    |
| Capacitor (Cordova)    | Logseq 0.10.15                                     | `assets/capacitor.config.json`, `assets/public/cordova.js`      | Works                                |
| Godot 4                | Pomodorot 0.13.3                                   | `libgodot_android.so`                                           | Works after the external-storage fix |
| Jetpack Compose        | Read You, Tomato, Droid-ify, FlorisBoard, ...      | `androidx/compose/ui` classes, `libandroidx.graphics.path.so`   | Works                                |
| libGDX / Arc / SDL 1.2 | Shattered Pixel Dungeon, Unciv, Mindustry, OpenTTD | `libgdx.so`, `libarc.so`, `libsdl-1.2.so`                       | Works                                |
| MapLibre Native (GLES) | StreetComplete 63.4, Wikipedia                     | `libmaplibre.so` (needs `libEGL`/`libGLESv3`)                   | Works                                |
| Xamarin/.NET MAUI      | none                                               | no FOSS Xamarin, MAUI or Avalonia app on F-Droid or IzzyOnDroid |                                      |
| Unity                  | none                                               | no FOSS Unity app on F-Droid or IzzyOnDroid                     |                                      |

### StreetComplete (MapLibre)

StreetComplete 63.4 (F-Droid, universal APK with x86_64 `libmaplibre.so`).
MapLibre Native's GL backend runs on ANGLE over RADV.

- **Script:** four onboarding pages, then the map.
  1. A two-finger pinch zooms in, a one-finger pan moves the map and a double
     tap zooms in. Each one is checked by comparing screenshots.
  2. A `geo:52.5163,13.3777?z=17` intent goes to street level at Pariser Platz.
- **Map:** vector tiles render at every zoom: world borders, CJK labels, street
  names, house numbers, one-way arrows.
- **Logs:** no GL/EGL/Vulkan errors, on both images. The only MapLibre messages
  are a few transient `Failed to load tile ... Connection reset` while panning.
- **Windowed run:** the same in its own Linux.apk window on a 2176x1360 display.
  The screenshot is on issue #1.
- **Manual checks still to do:**
  - mouse-wheel zoom and drag-pan with the host pointer (the harness injects
    touch);
  - quest pins after downloading an area;
  - answering a quest.

### Window types and app kinds

| App                 | What it exercises                  | What the smoke test checks                                                                        | Verify by hand later                                                              |
| ------------------- | ---------------------------------- | ------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| VLC 3.7.2           | picture-in-picture, media          | launch and first frame                                                                            | Play a video, press Home: does PiP get a window, can it move and close?           |
| NewPipe 0.29.1      | PiP and popup player, video        | dismisses its notice; Trending loads, no network error                                            | Popup player and PiP; does a stream play with sound?                              |
| Fennec 158.0.2      | GeckoView browser                  | through onboarding; a Wikipedia page loads (title in the a11y tree)                               | Multiple tabs and windows, downloads, upload picker, Gecko GPU process            |
| FlorisBoard 0.5.2   | input method                       | `ime enable` + `ime set` select it                                                                | Per-app windows hide soft keyboards (`persist.linuxapk.wm.ime=hide`); try `local` |
| Rethink 0.5.8       | `VpnService`                       | consent dialog (`com.android.vpndialogs`) → OK; a `VPN CONNECTED` network and a `tun` link appear | Browse through the VPN; firewall rules                                            |
| Floating Mute 4.0.1 | `SYSTEM_ALERT_WINDOW` overlay      | battery prompt answered; after Home its `APPLICATION_OVERLAY` window stays up                     | Does the floating button stay above other apps' windows and stay draggable?       |
| Mastodon 2.13.4     | OAuth sign-in in a browser         | Log in → mastodon.social → the sign-in page opens                                                 | Which browser should handle it, and does the redirect come back to the app?       |
| Jellyfin TV 0.19.10 | Android TV app (also has LAUNCHER) | launch and first frame ("optimized for televisions" notice)                                       | D-pad navigation from the host keyboard                                           |

## Fixed in this lane

1. **Launcher3 crash loop on the home screen.** On our 1280x800 display (961dp
   wide), Launcher3 shows the taskbar. With three-button navigation, the
   framework default, the bubble bar calls into `TaskbarNavButtonController`
   before its `init`, and every relaunch crashes again (about 5 per second):

   ```
   E AndroidRuntime: java.lang.NullPointerException: Attempt to read from field
     '...CueBarController ...TaskbarControllers.cueBarController' on a null object reference
     at ...TaskbarNavButtonController.onRecentsButtonLayoutChanged(TaskbarNavButtonController.java:289)
     at ...NavbarButtonsViewController.onBubbleBarLocationUpdated(NavbarButtonsViewController.java:1403)
   ```

   - **Symptoms:** `mCurrentFocus=null` and a constant stream of accessibility
     events, so `uiautomator dump` never got an idle state. Users would see a
     flickering home screen.
   - **Fix:** gesture navigation by default, in
     `session/rootfs-overlay/product/overlay/config/config.xml`. It stays
     mutable. `enable_bubble_bar` is a read-only aconfig flag, so it can't be
     turned off at runtime.
   - **Verified:** a fresh `/data` boots with `navigation_mode=2` and no
     `am_crash` from Launcher3.

2. **No network for apps.** `dumpsys connectivity` showed
   `Active default network: none`. pasta configured the interface, but no
   Android network agent owned it, so netd's rules ended in
   `32000: from all unreachable`.
   - **Interface:** `android.hardware.ethernet` (vendor permissions) starts
     EthernetService, and pasta names the interface `eth1`. With `eth0`, the
     image's resources made it a restricted network: no `NOT_RESTRICTED`, so no
     agent came up.
   - **DNS:** pasta only puts DNS in DHCP with `-D 169.254.1.1 --dhcp-dns`, and
     with `-D` set its forwarder needs an explicit `--dns-host`. The launcher
     reads that from the host's `resolv.conf`.
   - **Result:** the network is `IS_VALIDATED`, and Wikipedia loads its feed.
     `selftest` passes 12/12.
3. **Every MediaCodec failed to start.** Without the fix, SoundPool loads no
   sounds in Shattered Pixel Dungeon, Unciv or Solitaire, and media playback
   fails:

   ```
   W CCodecBufferChannel: [c2.android.mp3.decoder#946] start: cannot allocate memory at all
   E MediaCodec: Codec reported err 0xfffffff4/NO_MEMORY, actionCode 0, while in state 5/STARTING
   ```

   - **Cause:** Codec2 linear blocks default to ION or dma-buf heaps, and the
     session has neither.
   - **Fix:** `debug.stagefright.c2-poolmask=851968` (default | BLOB) in
     `init.linuxapk.rc` makes them gralloc BLOB buffers from our udmabuf
     allocator. MP3 decode now runs.

4. **App-specific external storage was missing on non-ext4 hosts.** Godot
   (Pomodorot) crashed at start:

   ```
   E vold: Failed to set project id on /data/media/0/Android/data/app.pomodorot/: Invalid argument
   E vold: Failed to set flags for /data/media/0/Android/data/app.pomodorot/ to set project id inheritance.: Invalid argument
   W ContextImpl: Failed to ensure /storage/emulated/0/Android/data/app.pomodorot/files
   E AndroidRuntime: Caused by: java.lang.NullPointerException: ... File.getCanonicalPath() on a null object reference
     at org.godotengine.godot.io.StorageScope$Identifier.<init>(StorageScope.kt:80)
   ```

   - **Cause:** vold tags app directories with a quota project id
     (`FS_IOC_FSSETXATTR`) and `FS_PROJINHERIT_FL`. In a user namespace, and on
     btrfs, both fail. vold only forgives that on ext4, so on other hosts
     `getExternalFilesDir()` returned null for every app.
   - **Fix:** the session shim (preloaded into vold and installd) treats the
     refused project-id ioctl as success, and sets the other flags without
     `FS_PROJINHERIT_FL`. This is what vold already does on ext4.

## Open issues, ranked

1. **Fixed (2026-10-27, gpu lane): video playback crashed SurfaceFlinger.**
   NewPipe's YV12 frames (`format:842094169`) made RenderEngine abort with
   `Failed to create a valid texture`, restarting all of Android. RADV didn't
   report YUV AHB formats to ANGLE. Mesa patch 0005 fixes it (see
   [gpu.md](gpu.md#yuv-buffers-video-and-the-surfaceflinger-abort)).
   - NewPipe now plays video with correct colors.
   - `compat/run.sh --window` gives 30/31 with no restarts (NewPipe works).

2. **arm64-only APKs don't install** (Open Sudoku 4.11.3; F-Droid has no x86_64
   build): `INSTALL_FAILED_NO_MATCHING_ABIS ... res=-113`. Next step: the M6
   NativeBridge. For M1, SudoQ stands in as the noarch Sudoku.
3. **lmkd can't lower `oom_score_adj`.** The log shows
   `E lowmemorykiller: Error writing /proc/<pid>/oom_score_adj; errno=13`
   (EACCES). Raising works, for example a cached process at 905, but lowering
   needs `CAP_SYS_RESOURCE` in the host user namespace.
   - **Effect:** a process that was cached keeps its high score after it comes
     back to the foreground. Under the session's `MemoryMax` cgroup, the kernel
     OOM killer can then pick the foreground app.
   - **Related:** zygote can't create per-app memcgs
     (`Activation of cgroup controller +memory failed in path /sys/fs/cgroup/apps/uid_N`).
   - **Lane:** session. Emulate the writes, or keep adj in lmkd only.
4. **OAuth and links open the WebView Shell.** The image's only browser is
   `org.chromium.webview_shell` ("WebView Browser Tester"), the default for
   `https:`. Mastodon's sign-in opens there, and there is no Custom Tabs
   provider. Whether its redirect comes back to the app is unchecked. A real
   default browser, or handing links to the host browser (M3), would fix this.
   Fennec offers itself as the default when it's installed.
5. **Legacy-target dialog.** Apps with targetSdk below 28, such as Simple
   Solitaire (25), open behind `DeprecatedTargetSdkVersionDialog`. This is stock
   Android behaviour. The harness dismisses the dialog, and a desktop build
   might want to suppress it.
6. **Soft keyboards don't show in per-app windows.** This is by design
   (`persist.linuxapk.wm.ime=hide`, the host keyboard types). An IME app like
   FlorisBoard can be enabled and selected, but it is never shown. Checking it
   needs `local` or `fallback`.
7. **Minor log noise, no visible effect:**
   - OpenTTD (SDL2):
     `ERROR: Setting the swap interval is not supported`/`GL_GetAttribute not supported`.
     Check `eglSwapInterval` on ANGLE.
   - `cutils-trace: Error opening trace file` in every app: there's no tracefs.
   - `BpfNetworkStats: failed to consume per uid Stats map`: eBPF is emulated.

## Not covered yet

- Installing apps from F-Droid or Aurora (PackageInstaller sessions and the
  unknown-sources flow). The M3 target.
- Input from a real host pointer (the harness uses `input` and the input
  bridge), and audio output.
- Deeper per-app scripts: AntennaPod subscribing to a feed, Thunderbird account
  setup, a game session past the menu.
