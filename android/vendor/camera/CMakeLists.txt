# SPDX-License-Identifier: AGPL-3.0-or-later
# The camera provider HAL (ICameraProvider AIDL V1), part of the root
# superbuild. The NDK build produces the vendor service. The native build
# (preset "host-tests") tests its scaling and JPEG encoding.
add_library(innkeeper_camera_convert STATIC src/Convert.cpp)
target_include_directories(innkeeper_camera_convert PUBLIC src)
target_compile_features(innkeeper_camera_convert PUBLIC cxx_std_20)
target_compile_options(innkeeper_camera_convert PRIVATE -Wall -Wextra -Werror)
set_target_properties(
  innkeeper_camera_convert
  PROPERTIES POSITION_INDEPENDENT_CODE ON)

if(NOT ANDROID)
  add_executable(camera_convert_test tests/convert_test.cpp)
  target_link_libraries(camera_convert_test PRIVATE innkeeper_camera_convert)
  add_test(NAME camera_convert COMMAND camera_convert_test)
  return()
endif()

set(GRAPHICS ${CMAKE_SOURCE_DIR}/android/vendor/graphics)
set(CAMERA_AIDL ${CMAKE_CURRENT_BINARY_DIR}/aidl)
innkeeper_gen_aidl(
  ${CAMERA_AIDL}
  android.hardware.common
  android.hardware.common.fmq
  android.hardware.graphics.common
  android.hardware.camera.common
  android.hardware.camera.metadata
  android.hardware.camera.device
  android.hardware.camera.provider)
file(GLOB_RECURSE CAMERA_AIDL_SRCS ${CAMERA_AIDL}/src/*.cpp)
add_library(camera_aidl STATIC ${CAMERA_AIDL_SRCS})
target_include_directories(
  camera_aidl
  PUBLIC ${CAMERA_AIDL}/include ${AOSP}/include)
target_compile_options(camera_aidl PRIVATE -w)
target_link_libraries(camera_aidl PUBLIC binder_ndk_stub log)

# Link-only stub for LL-NDK libcamera_metadata.
set(CM_STUB ${CMAKE_CURRENT_BINARY_DIR}/libcamera_metadata_stub)
add_custom_command(
  OUTPUT ${CM_STUB}.c ${CM_STUB}.ver
  COMMAND
    python3 ${VENDOR_TOOLS}/gen-stub.py ${AOSP}/libcamera_metadata.map.txt
    ${CM_STUB}
  DEPENDS ${VENDOR_TOOLS}/gen-stub.py ${AOSP}/libcamera_metadata.map.txt)
add_library(camera_metadata_stub SHARED ${CM_STUB}.c)
set_target_properties(
  camera_metadata_stub
  PROPERTIES
    OUTPUT_NAME camera_metadata LIBRARY_OUTPUT_DIRECTORY
    ${CMAKE_CURRENT_BINARY_DIR}/stubs C_VISIBILITY_PRESET default)
target_link_options(
  camera_metadata_stub
  PRIVATE -Wl,--version-script=${CM_STUB}.ver -nostdlib)

add_executable(
  camera
  src/main.cpp
  src/Provider.cpp
  src/Session.cpp
  src/HostCamera.cpp
  src/Buffers.cpp
  src/Metadata.cpp)
set_target_properties(
  camera
  PROPERTIES
    OUTPUT_NAME android.hardware.camera.provider-service.innkeeper CXX_STANDARD
    20)
target_include_directories(camera PRIVATE ${GRAPHICS}/common/include)
target_compile_options(camera PRIVATE -Wall -Wextra -Werror)
target_link_libraries(
  camera
  PRIVATE
    camera_aidl innkeeper_camera_convert innkeeper_bridge_client
    camera_metadata_stub binder_ndk_stub log)

set(VENDOR ${INNKEEPER_ANDROID}/vendor)
install(TARGETS camera RUNTIME DESTINATION ${VENDOR}/bin/hw)
install(
  FILES etc/init/android.hardware.camera.provider-service.innkeeper.rc
  DESTINATION ${VENDOR}/etc/init)
install(
  FILES etc/vintf/android.hardware.camera.provider-service.innkeeper.xml
  DESTINATION ${VENDOR}/etc/vintf/manifest)
