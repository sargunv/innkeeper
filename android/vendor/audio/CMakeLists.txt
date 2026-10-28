# SPDX-License-Identifier: AGPL-3.0-or-later
# libtinyalsav2.so: the tinyalsa API backed by the host bridge, replacing the
# image's copy in the AIDL audio HAL's APEX (docs/audio.md).
set(CMAKE_C_STANDARD 17)

add_library(
  tinyalsav2
  SHARED
    tinyalsa/pcm.c tinyalsa/mixer.c tinyalsa/audio_service.c tinyalsa/internal.c)
set_target_properties(tinyalsav2 PROPERTIES C_VISIBILITY_PRESET hidden)
target_include_directories(tinyalsav2 PRIVATE third_party/aosp/include)
target_compile_options(tinyalsav2 PRIVATE -Wall -Wextra -Werror)
target_link_options(
  tinyalsav2
  PRIVATE
    -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/tinyalsa/exports.map
    -Wl,-z,defs)
target_link_libraries(tinyalsav2 PRIVATE linuxapk_bridge_client log)

set(HAL_APEX ${LINUXAPK_ANDROID}/apex/com.android.hardware.audio)
install(
  TARGETS tinyalsav2
  LIBRARY DESTINATION ${HAL_APEX}/lib64 COMPONENT audio)
install(
  FILES
  etc/apex/com.android.hardware.audio/etc/android.hardware.audio.service-aidl.example.rc
  DESTINATION ${HAL_APEX}/etc
  COMPONENT audio)
install(
  FILES etc/primary_audio_policy_configuration.xml
  DESTINATION ${LINUXAPK_ANDROID}/vendor/etc
  COMPONENT audio)
install(FILES etc/audio.prop DESTINATION share/linuxapk/props.d COMPONENT audio)
