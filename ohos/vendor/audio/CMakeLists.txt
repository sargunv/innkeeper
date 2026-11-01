# SPDX-License-Identifier: AGPL-3.0-or-later
# Host unit tests of OpenHarmony's audio VDI. ohos/build.sh builds the VDI
# itself for OH's musl.
add_executable(ohos_audio_vdi_test tests/vdi_test.c primary.c)
target_link_libraries(
  ohos_audio_vdi_test
  PRIVATE innkeeper_audio_client pthread m)
target_compile_options(
  ohos_audio_vdi_test
  PRIVATE -Wall -Wextra -Werror -UNDEBUG)
set_target_properties(ohos_audio_vdi_test PROPERTIES C_STANDARD 17)
add_test(NAME ohos_audio_vdi COMMAND ohos_audio_vdi_test)
