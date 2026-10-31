# image/ohos

The OpenHarmony image the OH guest ([ohos/](../../ohos/README.md)) boots:
Oniro's x86_64 emulator release, built on OpenHarmony 7.0-Release
([pin.toml](pin.toml)). It is downloaded from GitHub on the developer's machine
and never redistributed or committed. No OpenHarmony source publishes an
anonymous x86_64 standard-system image, so this is the closest to Android's CI
pin (#191).

`fetch.sh` downloads the zip (resumable, size and SHA-256 checked), then
extracts `system.img` and `vendor.img` with `image/fs-extract.sh`'s static
debugfs, read-only as on a mount: no loop mounts, no root. The guest kernel
(`bzImage`), ramdisk, updater, `eng_system` and `userdata` images are discarded;
the guest runs on the host kernel in our sandbox, never in QEMU.
`mise run ohos:test` runs it.

Cache in `out/ohos/images/<name>/`: `dl/` (the zip, deleted once extracted) and
`img/` (`system/`, system.img's system-as-root tree, and `vendor/`). About 0.9
GB downloaded, 1.9 GB extracted.

## Bumping the pin

Take a newer `oniro_emulator.zip` from
[device_board_oniro releases](https://github.com/eclipse-oniro4openharmony/device_board_oniro/releases)
(`gh release view <tag> --json assets` gives the size and digest), update
`name`, `version`, `url`, `size` and `sha256`, then run `mise run ohos:test`.
