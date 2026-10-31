# SPDX-License-Identifier: AGPL-3.0-or-later
# The audio_bridge AIDL module, installed into the AIDL audio HAL's APEX
# overlay (README.md).
set(CMAKE_C_STANDARD 17)
# The bridge's audio service and pcm.h's blocking device over it.
add_library(innkeeper_audio_client STATIC stream/audio.c stream/pcm.c)
set_target_properties(
  innkeeper_audio_client
  PROPERTIES C_VISIBILITY_PRESET hidden POSITION_INDEPENDENT_CODE ON)
target_include_directories(innkeeper_audio_client PUBLIC stream)
target_compile_options(innkeeper_audio_client PRIVATE -Wall -Wextra -Werror)
target_link_libraries(innkeeper_audio_client PUBLIC innkeeper_bridge_client)
if(ANDROID)
  target_link_libraries(innkeeper_audio_client PUBLIC log)
endif()
set(AUDIO_AIDL ${CMAKE_CURRENT_BINARY_DIR}/aidl)
innkeeper_gen_aidl(
  ${AUDIO_AIDL}
  android.hardware.common
  android.hardware.common.fmq
  android.hardware.audio.common
  android.hardware.audio.core
  android.hardware.audio.core.sounddose
  android.hardware.audio.effect
  android.media.audio.common.types
  android.media.audio.eraser.types)

if(NOT ANDROID)
  add_executable(audio_output_test tests/output_test.cpp)
  target_include_directories(audio_output_test PRIVATE stream)
  target_compile_features(audio_output_test PRIVATE cxx_std_20)
  add_test(NAME audio_output COMMAND audio_output_test)
  add_executable(audio_usage_test tests/usage_test.cpp)
  target_include_directories(audio_usage_test PRIVATE stream)
  target_compile_features(audio_usage_test PRIVATE cxx_std_20)
  add_test(NAME audio_usage COMMAND audio_usage_test)
  add_executable(audio_queue_test tests/queue_test.cpp)
  target_include_directories(
    audio_queue_test
    PRIVATE stream ${AUDIO_AIDL}/include ${AOSP}/include)
  target_compile_features(audio_queue_test PRIVATE cxx_std_20)
  target_link_libraries(audio_queue_test PRIVATE pthread)
  add_test(NAME audio_queue COMMAND audio_queue_test)
  set_tests_properties(audio_queue PROPERTIES TIMEOUT 5)
  add_executable(pcm_capture_test tests/capture_test.c)
  target_link_libraries(pcm_capture_test PRIVATE innkeeper_audio_client pthread)
  target_compile_options(
    pcm_capture_test
    PRIVATE -Wall -Wextra -Werror -UNDEBUG)
  add_test(NAME pcm_capture COMMAND pcm_capture_test)
  add_executable(audio_client_test tests/audio_client_test.c)
  target_link_libraries(
    audio_client_test
    PRIVATE innkeeper_audio_client pthread)
  target_compile_options(audio_client_test PRIVATE -UNDEBUG)
  add_test(NAME audio_client COMMAND audio_client_test)
  return()
endif()

set(HAL_APEX ${INNKEEPER_ANDROID}/apex/com.android.hardware.audio)
install(
  FILES
  etc/apex/com.android.hardware.audio/etc/android.hardware.audio.service-aidl.example.rc
  DESTINATION ${HAL_APEX}/etc
  COMPONENT audio)
install(
  FILES etc/primary_audio_policy_configuration.xml
  DESTINATION ${INNKEEPER_ANDROID}/vendor/etc
  COMPONENT audio)
install(
  FILES etc/audio.prop
  DESTINATION share/innkeeper/props.d
  COMPONENT audio)

file(GLOB_RECURSE AUDIO_AIDL_SRCS ${AUDIO_AIDL}/src/*.cpp)
add_library(audio_aidl STATIC ${AUDIO_AIDL_SRCS})
target_include_directories(
  audio_aidl
  PUBLIC ${AUDIO_AIDL}/include ${AOSP}/include)
target_compile_options(audio_aidl PRIVATE -w)
target_link_libraries(audio_aidl PUBLIC binder_ndk_stub log)
add_executable(audio_bridge stream/Module.cpp stream/StreamBridge.cpp
               stream/StreamInBridge.cpp)
target_compile_features(audio_bridge PRIVATE cxx_std_20)
target_link_libraries(audio_bridge PRIVATE audio_aidl innkeeper_audio_client)
add_executable(audio_input_test tests/input_test.cpp stream/StreamInBridge.cpp)
target_compile_features(audio_input_test PRIVATE cxx_std_20)
target_include_directories(audio_input_test PRIVATE stream)
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
  DESTINATION ${INNKEEPER_ANDROID}/vendor/etc/vintf/manifest
  COMPONENT audio)
