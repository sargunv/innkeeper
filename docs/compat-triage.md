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

Run it as a capped job:
`systemd-run --user --unit=compat-run --collect -p MemoryMax=4G -- compat/run.sh`.

- **Corpus:** `compat/corpus.toml` holds pinned F-Droid APKs (version code, URL
  and SHA-256). They download into `out/corpus/` and are never committed.
- **Per app** (`compat/src/app.rs`):
  1. Copy the APK into `/data/local/tmp` and run `pm install -r -g`.
  2. Resolve the launcher activity, then `am start -W` (start time).
  3. For 8 s (15 s for games), poll the crash buffer, ActivityManager ANRs and
     `pidof`.
  4. Check that the app's window has focus. A platform "built for an older
     version of Android" dialog is noted and dismissed.
  5. `screencap -p`. Below the status bar, the frame is blank if one colour
     covers more than 99.5% of it.
  6. Run the optional per-app script: tap or find text through
     `uiautomator dump`.
  7. Tap the centre, scroll and press Back. After each one, check the app is
     still alive.
  8. Scan the log for known failure signatures: MediaCodec start errors, missing
     WebView, Google Play services.
  9. Save bounded logs and screenshots into `out/compat/<pkg>/`, then uninstall.
- **Result:** "works" means everything passed. "Partly works" means the first
  frame was fine but a later check failed. "Doesn't work" covers install,
  launch, early death, a blank frame or a dialog in front. Session restarts
  during an app are retried once and never count against the app.
- **Gate:** any app whose result is worse than in `compat/results.json` fails
  the run (exit 1), and the baseline is left alone. `--accept` records it
  anyway.
- **Session:** the harness uses `session/linuxapk-session` (from a lane
  worktree, the main checkout's). If no session is running, it starts a headless
  one and waits for `sys.boot_completed`. At the end it leaves a session showing
  its window (`--no-finish` skips that).

## Results (2026-10-27)

15 of 16 apps work: every noarch, Compose and x86_64-native app in the corpus.
They install, render, survive input, reach the network (Wikipedia's feed and
articles in WebView, F-Droid's repository fetch) and decode media.

Screenshots: see the README (images are hosted as attachments on issue #1, not
committed).

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

## Open issues, ranked

1. **arm64-only APKs don't install** (Open Sudoku 4.11.3; F-Droid has no x86_64
   build): `INSTALL_FAILED_NO_MATCHING_ABIS ... res=-113`. Next step: the M6
   NativeBridge. For M1, SudoQ stands in as the noarch Sudoku.
2. **lmkd can't lower `oom_score_adj`.** The log shows
   `E lowmemorykiller: Error writing /proc/<pid>/oom_score_adj; errno=13`
   (EACCES). Raising works, for example a cached process at 905, but lowering
   needs `CAP_SYS_RESOURCE` in the host user namespace.
   - **Effect:** a process that was cached keeps its high score after it comes
     back to the foreground. Under the session's `MemoryMax` cgroup, the kernel
     OOM killer can then pick the foreground app.
   - **Related:** zygote can't create per-app memcgs
     (`Activation of cgroup controller +memory failed in path /sys/fs/cgroup/apps/uid_N`).
   - **Lane:** session. Emulate the writes, or keep adj in lmkd only.
3. **No sound output.** Decoding works now, but audio still goes to the
   emulator's stub HAL. Lane: the planned PipeWire audio HAL.
4. **Legacy-target dialog.** Apps with targetSdk below 28, such as Simple
   Solitaire (25), open behind `DeprecatedTargetSdkVersionDialog`. This is stock
   Android behaviour. The harness dismisses the dialog, and a desktop build
   might want to suppress it.
5. **Minor log noise, no visible effect:**
   - OpenTTD (SDL2):
     `ERROR: Setting the swap interval is not supported`/`GL_GetAttribute not supported`.
     Check `eglSwapInterval` on ANGLE.
   - `cutils-trace: Error opening trace file` in every app: there's no tracefs.
   - `BpfNetworkStats: failed to consume per uid Stats map`: eBPF is emulated.

## Not covered yet

- Installing apps from F-Droid or Aurora (PackageInstaller sessions and the
  unknown-sources flow). The M3 target.
- Input from a real host pointer (the harness uses `input`), and audio.
- Deeper per-app scripts: AntennaPod subscribing to a feed, Thunderbird account
  setup, a game session past the menu.
