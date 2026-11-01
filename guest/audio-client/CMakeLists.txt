# SPDX-License-Identifier: AGPL-3.0-or-later
# libinnkeeper_audio_client.a is the client of the bridge's audio service and
# pcm.h's blocking device over it, for the guests' audio HALs. It installs
# nothing. A native build (preset "host-tests") also builds its tests.
add_library(innkeeper_audio_client STATIC audio.c pcm.c)
set_target_properties(
  innkeeper_audio_client
  PROPERTIES
    C_STANDARD 17 C_VISIBILITY_PRESET hidden POSITION_INDEPENDENT_CODE ON)
target_include_directories(innkeeper_audio_client PUBLIC include)
target_compile_options(innkeeper_audio_client PRIVATE -Wall -Wextra -Werror)
target_link_libraries(innkeeper_audio_client PUBLIC innkeeper_bridge_client)
if(ANDROID)
  target_link_libraries(innkeeper_audio_client PUBLIC log)
else()
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
endif()
