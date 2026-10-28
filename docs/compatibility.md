# Compatibility

Apps from the F-Droid corpus (`tests/compat/corpus.toml`), smoke-tested by the compat harness (`mise run test:compat`): install, launch, a non-blank first frame, then a tap, a scroll and Back without a crash or ANR, plus an optional per-app script. Failures are triaged in [tests/compat](../tests/compat/README.md). Generated: run `mise run test:compat` (or `mise run test:compat -- report` to rewrite this file from `tests/compat/results.json`). Start is the time to the first frame on a fresh install; memory is the app's total PSS after the interactions.

Last run: 2026-10-28, image `generic/aosp_cf_x86_64_only_phone/vsoc_x86_64_only:17/CP2A.260605.016/16644040:userdebug/test-keys`, headless session. 31 apps: 30 work, 0 partly, 1 don't, 0 not tested.

| App | Version | Framework | Native code | Result | Start | Memory | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| OpenCalc | 3.2.0 | Android Views | none | Works | 104 ms | 29 MB |  |
| Open Sudoku | 4.11.3 | Android Views | arm64-v8a only | Doesn't work |  |  | install: INSTALL_FAILED_NO_MATCHING_ABIS: Failed to extract native libraries, res=-113 |
| SudoQ | 3.3.3 | Android Views | none | Works | 2667 ms | 25 MB |  |
| Unit Converter Ultimate | 5.7.3 | Android Views | none | Works | 104 ms | 37 MB |  |
| Simple Solitaire Collection | 3.13 | Android Views | none | Works | 144 ms | 56 MB |  |
| Wikipedia | r/50612-r-2026-10-19 | Views + Compose, MapLibre | x86_64 | Works | 194 ms | 122 MB |  |
| AntennaPod - Podcast Player | 3.12.3 | Android Views | x86_64 | Works | 187 ms | 59 MB |  |
| Thunderbird: Free Your Inbox | 24.0 | Jetpack Compose | x86_64 | Works | 241 ms | 44 MB |  |
| Aurora Store | 4.8.4 | Jetpack Compose | x86_64 | Works | 184 ms | 44 MB |  |
| F-Droid | 2.0.2 | Jetpack Compose | x86_64 | Works | 264 ms | 105 MB |  |
| Read You | 0.16.2 | Jetpack Compose | x86_64 | Works | 193 ms | 59 MB |  |
| Tomato | 2.0.1 | Jetpack Compose | x86_64 | Works | 301 ms | 59 MB |  |
| Mindustry | 8-fdroid-160.7 | Arc (libGDX-like) | x86_64 | Works | 293 ms | 117 MB |  |
| Shattered Pixel Dungeon | 4.0.2 | libGDX | x86_64 | Works | 141 ms | 53 MB |  |
| Unciv | 4.22.13 | libGDX | x86_64 | Works | 242 ms | 66 MB |  |
| OpenTTD | 14.1.rev128 | SDL 1.2 (C++) | x86_64 | Works | 87 ms | 233 MB |  |
| StreetComplete | 63.4 | Compose + MapLibre Native (GLES) | x86_64 | Works | 217 ms | 153 MB |  |
| Saber | 1.36.3 | Flutter | x86_64 | Works | 212 ms | 107 MB |  |
| Ente Auth - 2FA Authenticator | 4.4.26 | Flutter | x86_64 | Works | 404 ms | 107 MB |  |
| Joplin | 3.7.8 | React Native (Hermes) | x86_64 | Works | 113 ms | 234 MB |  |
| Logseq | 0.10.15 | Capacitor (WebView) | x86_64 | Works | 734 ms | 95 MB |  |
| Pomodorot | 0.13.3 | Godot 4 | x86_64 | Works | 114 ms | 180 MB |  |
| Droid-ify | 0.7.8 | Jetpack Compose | x86_64 | Works | 168 ms | 47 MB |  |
| VLC | 3.7.2 | Android Views + libVLC | x86_64 | Works | 104 ms | 77 MB |  |
| NewPipe | 0.29.1 | Android Views | x86_64 | Works | 169 ms | 147 MB |  |
| Fennec F-Droid | 158.0.2 | GeckoView + Compose | x86_64 | Works | 187 ms | 482 MB |  |
| FlorisBoard | 0.5.2 | Jetpack Compose (IME) | x86_64 | Works | 169 ms | 40 MB |  |
| Rethink: DNS + Firewall + VPN | v0.5.8 | Android Views + Go | x86_64 | Works | 154 ms | 97 MB |  |
| Jellyfin for Android TV | 0.19.10 | Leanback / Compose for TV | x86_64 | Works | 123 ms | 90 MB |  |
| Mastodon | 2.13.4 | Android Views | none | Works | 110 ms | 45 MB |  |
| Floating Mute | 4.0.1 | Android Views (overlay) | none | Works | 181 ms | 32 MB |  |

Session memory (cgroup) at the end of the run: 645 MiB.

## Budgets

A run fails when a measurement is over its budget by more than the margin (`tests/compat/budgets.toml`). Cold session start is `start` to `sys.boot_completed` with no session running, on a `/data` that has booted before. Warm app start is the median start of the apps that work, in the running session (full runs only). Idle session memory is the session cgroup's memory without page cache, 90 s after a cold start. Session start and idle memory are measured only when the run starts the session itself.

| Measure | Budget | Margin | Fails above | Last measured |
| --- | --- | --- | --- | --- |
| cold session start | 2500 ms | 25% | 3125 ms | 2282 ms |
| warm app start | 200 ms | 25% | 250 ms | 182 ms |
| idle session memory | 575 MiB | 10% | 633 MiB | 560 MiB |
