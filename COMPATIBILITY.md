# Compatibility

Apps from the F-Droid corpus (`compat/corpus.toml`), smoke-tested by the compat harness (`compat/run.sh`): install, launch, a non-blank first frame, then a tap, a scroll and Back without a crash or ANR, plus an optional per-app script. Failures are triaged in [docs/compat-triage.md](docs/compat-triage.md). Generated: run `compat/run.sh` (or `compat/run.sh report` to rewrite this file from `compat/results.json`). Start is `am start -W` TotalTime on a fresh install; memory is the app's total PSS after the interactions.

Last run: 2026-10-27, image `generic/aosp_cf_x86_64_only_phone/vsoc_x86_64_only:17/CP2A.260605.016/16644040:userdebug/test-keys`, headless composer. 31 apps: 30 work, 0 partly, 1 don't, 0 not tested.

| App | Version | Framework | Native code | Result | Start | Memory | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| OpenCalc | 3.2.0 | Android Views | none | Works | 104 ms | 28 MB |  |
| Open Sudoku | 4.11.3 | Android Views | arm64-v8a only | Doesn't work |  |  | install: INSTALL_FAILED_NO_MATCHING_ABIS: Failed to extract native libraries, res=-113 |
| SudoQ | 3.3.3 | Android Views | none | Works | 2680 ms | 21 MB |  |
| Unit Converter Ultimate | 5.7.3 | Android Views | none | Works | 117 ms | 33 MB |  |
| Simple Solitaire Collection | 3.13 | Android Views | none | Works | 149 ms | 49 MB |  |
| Wikipedia | r/50612-r-2026-10-19 | Views + Compose, MapLibre | x86_64 | Works | 213 ms | 113 MB |  |
| AntennaPod - Podcast Player | 3.12.3 | Android Views | x86_64 | Works | 184 ms | 51 MB |  |
| Thunderbird: Free Your Inbox | 24.0 | Jetpack Compose | x86_64 | Works | 244 ms | 36 MB |  |
| Aurora Store | 4.8.4 | Jetpack Compose | x86_64 | Works | 180 ms | 36 MB |  |
| F-Droid | 2.0.2 | Jetpack Compose | x86_64 | Works | 289 ms | 95 MB |  |
| Read You | 0.16.2 | Jetpack Compose | x86_64 | Works | 173 ms | 63 MB |  |
| Tomato | 2.0.1 | Jetpack Compose | x86_64 | Works | 279 ms | 52 MB |  |
| Mindustry | 8-fdroid-160.7 | Arc (libGDX-like) | x86_64 | Works | 259 ms | 107 MB |  |
| Shattered Pixel Dungeon | 4.0.2 | libGDX | x86_64 | Works | 146 ms | 44 MB |  |
| Unciv | 4.22.13 | libGDX | x86_64 | Works | 244 ms | 57 MB |  |
| OpenTTD | 14.1.rev128 | SDL 1.2 (C++) | x86_64 | Works | 89 ms | 225 MB |  |
| StreetComplete | 63.4 | Compose + MapLibre Native (GLES) | x86_64 | Works | 224 ms | 144 MB |  |
| Saber | 1.36.3 | Flutter | x86_64 | Works | 192 ms | 98 MB |  |
| Ente Auth - 2FA Authenticator | 4.4.26 | Flutter | x86_64 | Works | 323 ms | 99 MB |  |
| Joplin | 3.7.8 | React Native (Hermes) | x86_64 | Works | 95 ms | 225 MB |  |
| Logseq | 0.10.15 | Capacitor (WebView) | x86_64 | Works | 741 ms | 88 MB |  |
| Pomodorot | 0.13.3 | Godot 4 | x86_64 | Works | 94 ms | 172 MB |  |
| Droid-ify | 0.7.8 | Jetpack Compose | x86_64 | Works | 178 ms | 61 MB |  |
| VLC | 3.7.2 | Android Views + libVLC | x86_64 | Works | 119 ms | 69 MB |  |
| NewPipe | 0.29.1 | Android Views | x86_64 | Works | 172 ms | 139 MB |  |
| Fennec F-Droid | 158.0.2 | GeckoView + Compose | x86_64 | Works | 190 ms | 458 MB |  |
| FlorisBoard | 0.5.2 | Jetpack Compose (IME) | x86_64 | Works | 90 ms | 32 MB |  |
| Rethink: DNS + Firewall + VPN | v0.5.8 | Android Views + Go | x86_64 | Works | 91 ms | 87 MB |  |
| Jellyfin for Android TV | 0.19.10 | Leanback / Compose for TV | x86_64 | Works | 205 ms | 82 MB |  |
| Mastodon | 2.13.4 | Android Views | none | Works | 129 ms | 37 MB |  |
| Floating Mute | 4.0.1 | Android Views (overlay) | none | Works | 122 ms | 24 MB |  |

Session memory (cgroup) at the end of the run: 872 MiB.
