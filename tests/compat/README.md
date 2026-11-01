# compat

The compat harness is a manual tool for troubleshooting apps. It is not a gate
for PRs or agent loops. The crate is `innkeeper-compat`, and you run it with
`mise run test:compat -- --only <package>,...`. The header of `src/main.rs`
documents its usage. The harness installs each selected app from
[corpus.toml](corpus.toml) into a session. The corpus pins F-Droid APKs, which
are fetched to `out/test/compat/corpus` and never committed. The harness writes
`out/test/compat/<profile>/results.json`, and the run fails unless every app
works.

`src/app.rs` runs these checks for each app:

1. Install the app with `pm install` and launch it through the window manager.
2. Watch for crashes, ANRs and process death for the app's settle time, which is
   8 s by default and longer for some games.
3. Check window focus.
4. Take a screenshot and check that it is not blank. A screenshot is blank when
   one colour covers more than 99.5% of it.
5. Run the app's optional script. The script syntax is described at the top of
   `corpus.toml`.
6. Tap, scroll and press Back.
7. Scan the log for known failure signatures, such as MediaCodec, WebView,
   GL/EGL/Vulkan and Play services errors.

Logs and screenshots go to `out/test/compat/<profile>/<pkg>/`. When a script
fails, the last UI dump is saved there as `ui.xml`.

Compat audio goes to a private null sink named `innkeeper_compat_<profile>`.
`INNKEEPER_AUDIO_SINK` overrides the name. The `audio:` script step records up
to three seconds of the sink's monitor and checks that the PCM is louder than
-60 dBFS. The capture is kept in `audio.raw` and the level analysis in
`audio.txt`. Run this step while the app's background music plays, before any
taps or sound effects. A session started outside the harness must use the same
sink. A run needs `pactl` and `parec`.

- **Results.** Each app works, partly works, or doesn't work. An app partly
  works when the first frame was fine and a later check failed. If the session
  restarts during an app, the harness retries the app once and doesn't count the
  restart against it.
- **Session.** The harness uses the profile named by `INNKEEPER_PROFILE`, which
  defaults to `compat`. If that profile isn't running, the harness starts it and
  stops it at the end. The session is headless unless you pass `--window`. A
  `--window` run gets its own nested desktop (`tests/nested.sh`). The session
  runs as a test session (`INNKEEPER_TEST=1`), so links are logged and not
  opened.
- **Niceness.** uiautomator dies when it can't reset its thread priority. When
  the harness finds itself niced, it runs itself again at nice 0.
- **Frameworks.** The corpus lists each app's framework, identified by hand from
  the APK's libraries and assets: `libflutter.so`, `libhermes.so`,
  `assets/capacitor.config.json`, `libgodot_android.so`, Compose classes,
  `libgdx.so` and `libmaplibre.so`. No FOSS Unity or Xamarin app is available to
  test.
- **Triage.** Look for a failing app's failure signature in its
  `out/test/compat/<profile>/<pkg>/` logs. The `manual` notes in the corpus list
  checks that the harness doesn't automate yet.
- **VPN apps.** The sandbox has no TUN device, so Rethink's VPN check fails and
  the app partly works.
