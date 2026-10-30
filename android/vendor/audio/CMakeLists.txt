# SPDX-License-Identifier: AGPL-3.0-or-later
# libtinyalsav2.so: the tinyalsa API backed by the host bridge, replacing the
# image's copy in the AIDL audio HAL's APEX (README.md).
set(CMAKE_C_STANDARD 17)
set(AUDIO_AIDL ${CMAKE_CURRENT_BINARY_DIR}/aidl)
file(GLOB_RECURSE AUDIO_AIDL_INPUTS CONFIGURE_DEPENDS
     third_party/aosp/aidl/*.aidl)
set_property(
  DIRECTORY
  APPEND
  PROPERTY CMAKE_CONFIGURE_DEPENDS ${AUDIO_AIDL_INPUTS})
execute_process(
  COMMAND
    ${CMAKE_CURRENT_SOURCE_DIR}/../graphics/tools/gen-aidl.sh ${AUDIO_AIDL}
    ${CMAKE_CURRENT_SOURCE_DIR}/third_party/aosp/aidl OUTPUT_QUIET
  COMMAND_ERROR_IS_FATAL ANY)

if(NOT ANDROID)
  add_executable(audio_output_test tests/output_test.cpp)
  target_include_directories(audio_output_test PRIVATE stream)
  target_compile_features(audio_output_test PRIVATE cxx_std_20)
  add_test(NAME audio_output COMMAND audio_output_test)
  add_executable(audio_queue_test tests/queue_test.cpp)
  target_include_directories(
    audio_queue_test
    PRIVATE
      stream ${AUDIO_AIDL}/include
      ${CMAKE_CURRENT_SOURCE_DIR}/../graphics/third_party/aosp/include)
  target_compile_features(audio_queue_test PRIVATE cxx_std_20)
  target_link_libraries(audio_queue_test PRIVATE pthread)
  add_test(NAME audio_queue COMMAND audio_queue_test)
  set_tests_properties(audio_queue PROPERTIES TIMEOUT 5)
  add_executable(pcm_capture_test tests/capture_test.c tinyalsa/pcm.c)
  target_include_directories(pcm_capture_test PRIVATE third_party/aosp/include)
  target_link_libraries(pcm_capture_test PRIVATE linuxapk_bridge_client pthread)
  target_compile_options(
    pcm_capture_test
    PRIVATE -Wall -Wextra -Werror -UNDEBUG)
  add_test(NAME pcm_capture COMMAND pcm_capture_test)
  return()
endif()

add_library(
  tinyalsav2
  SHARED tinyalsa/pcm.c tinyalsa/mixer.c tinyalsa/internal.c)
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

if(ANDROID)
  file(GLOB_RECURSE AUDIO_AIDL_SRCS ${AUDIO_AIDL}/src/*.cpp)
  add_library(audio_aidl STATIC ${AUDIO_AIDL_SRCS})
  target_include_directories(
    audio_aidl
    PUBLIC
      ${AUDIO_AIDL}/include
      ${CMAKE_CURRENT_SOURCE_DIR}/../graphics/third_party/aosp/include)
  target_compile_options(audio_aidl PRIVATE -w)
  target_link_libraries(audio_aidl PUBLIC binder_ndk_stub log)
  add_executable(audio_bridge stream/Module.cpp stream/StreamBridge.cpp
                 stream/StreamInBridge.cpp)
  target_compile_features(audio_bridge PRIVATE cxx_std_20)
  target_include_directories(audio_bridge PRIVATE third_party/aosp/include)
  target_link_libraries(audio_bridge PRIVATE audio_aidl tinyalsav2)
  add_executable(
    audio_input_test tests/input_test.cpp stream/StreamInBridge.cpp)
  target_compile_features(audio_input_test PRIVATE cxx_std_20)
  target_include_directories(
    audio_input_test
    PRIVATE stream third_party/aosp/include)
  target_link_libraries(audio_input_test PRIVATE audio_aidl)
  set_target_properties(
    audio_input_test
    PROPERTIES RUNTIME_OUTPUT_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/test-bin)
  install(
    TARGETS audio_bridge
    RUNTIME DESTINATION ${HAL_APEX}/bin/hw COMPONENT audio)
  install(FILES etc/audio-bridge.rc DESTINATION ${HAL_APEX}/etc COMPONENT audio)
  install(
    FILES etc/audio-bridge.xml
    DESTINATION ${LINUXAPK_ANDROID}/vendor/etc/vintf/manifest
    COMPONENT audio)
endif()
