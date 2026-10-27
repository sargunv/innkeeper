# Compatibility

Apps from the F-Droid corpus (`compat/corpus.toml`), smoke-tested by the compat harness (`compat/run.sh`): install, launch, a non-blank first frame, then a tap, a scroll and Back without a crash or ANR, plus an optional per-app script. Failures are triaged in [docs/compat-triage.md](docs/compat-triage.md). Generated: run `compat/run.sh` (or `compat/run.sh report` to rewrite this file from `compat/results.json`). Start is `am start -W` TotalTime on a fresh install; memory is the app's total PSS after the interactions.

Last run: 2026-10-27, image `Android/sdk_phone64_x86_64/emu64x:17/CE2A.260420.051.A3/16571990:userdebug/test-keys`, headless composer. 16 apps: 15 work, 0 partly, 1 don't, 0 not tested.

| App | Version | Code | Result | Start | Memory | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| OpenCalc | 3.2.0 | no native code | Works | 112 ms | 33 MB |  |
| Open Sudoku | 4.11.3 | arm64-v8a libs only | Doesn't work |  |  | install: Failure [INSTALL_FAILED_NO_MATCHING_ABIS: INSTALL_FAILED_NO_MATCHING_ABIS: Failed to extract native libraries, res=-113] |
| SudoQ | 3.3.3 | no native code | Works | 2645 ms | 26 MB |  |
| Unit Converter Ultimate | 5.7.3 | no native code | Works | 96 ms | 38 MB |  |
| Simple Solitaire Collection | 3.13 | no native code | Works | 129 ms | 53 MB |  |
| Wikipedia | r/50612-r-2026-10-19 | x86_64 libs | Works | 176 ms | 60 MB |  |
| AntennaPod - Podcast Player | 3.12.3 | x86_64 libs | Works | 188 ms | 57 MB |  |
| Thunderbird: Free Your Inbox | 24.0 | x86_64 libs | Works | 209 ms | 50 MB |  |
| Aurora Store | 4.8.4 | x86_64 libs | Works | 176 ms | 50 MB |  |
| F-Droid | 2.0.2 | x86_64 libs | Works | 257 ms | 97 MB |  |
| Read You | 0.16.2 | Compose, x86_64 libs | Works | 190 ms | 67 MB |  |
| Tomato | 2.0.1 | Compose, x86_64 libs | Works | 273 ms | 55 MB |  |
| Mindustry | 8-fdroid-160.7 | x86_64 libs | Works | 139 ms | 114 MB |  |
| Shattered Pixel Dungeon | 4.0.2 | x86_64 libs | Works | 112 ms | 46 MB |  |
| Unciv | 4.22.13 | x86_64 libs | Works | 129 ms | 62 MB |  |
| OpenTTD | 14.1.rev128 | x86_64 libs | Works | 100 ms | 233 MB |  |

Session memory (cgroup) at the end of the run: 901 MiB.
