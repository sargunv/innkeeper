# SPDX-License-Identifier: AGPL-3.0-or-later
# Linux.apk display HALs. Configure twice (see build.sh):
#   - with the NDK toolchain: vendor binaries for the Android session;
#   - natively: host (glibc) unit tests of the buffer/mapper code.
cmake_minimum_required(VERSION 3.22)
project(linuxapk_hal C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)
add_compile_options(-Wall -Wextra -Wno-unused-parameter
                    -Wno-missing-field-initializers)

if(NOT AIDL_GEN_DIR)
  message(
    FATAL_ERROR
      "AIDL_GEN_DIR not set (run tools/gen-aidl.sh; build.sh does this)")
endif()

set(HAL ${CMAKE_CURRENT_SOURCE_DIR})
set(HAL_INCLUDES ${HAL}/common/include ${HAL}/third_party/aosp/include
    ${AIDL_GEN_DIR}/include)
if(NOT ANDROID)
  # glibc host: shims for headers that assume Bionic.
  list(APPEND HAL_INCLUDES ${HAL}/tests/host/compat)
  add_compile_options(-include ${HAL}/tests/host/compat/host_compat.h)
endif()

add_library(
  lapk_common
  STATIC
    common/src/native_handle.cpp common/src/layout.cpp
    common/src/buffer_alloc.cpp)
target_include_directories(lapk_common PUBLIC ${HAL_INCLUDES})
if(ANDROID)
  target_link_libraries(lapk_common PUBLIC log)
endif()

# IMapper v5: exports only AIMapper_loadIMapper + ANDROID_HAL_STABLEC_VERSION.
add_library(mapper SHARED mapper/mapper.cpp)
set_target_properties(mapper PROPERTIES PREFIX "" OUTPUT_NAME "mapper.linuxapk")
target_link_libraries(mapper PRIVATE lapk_common)
target_link_options(
  mapper
  PRIVATE
    -Wl,--version-script=${HAL}/mapper/mapper.map -Wl,--exclude-libs,ALL
    -Wl,--no-undefined)

if(ANDROID)
  # Link-only stub for LL-NDK libbinder_ndk, including platform symbols that
  # the public NDK stub omits (AServiceManager_addService, ...).
  set(STUB ${CMAKE_CURRENT_BINARY_DIR}/libbinder_ndk_stub)
  add_custom_command(
    OUTPUT ${STUB}.c ${STUB}.ver
    COMMAND
      python3 ${HAL}/stubs/gen_stub.py
      ${HAL}/third_party/aosp/libbinder_ndk.map.txt ${STUB}
    DEPENDS
      ${HAL}/stubs/gen_stub.py ${HAL}/third_party/aosp/libbinder_ndk.map.txt)
  add_library(binder_ndk_stub SHARED ${STUB}.c)
  set_target_properties(
    binder_ndk_stub
    PROPERTIES
      OUTPUT_NAME binder_ndk LIBRARY_OUTPUT_DIRECTORY
      ${CMAKE_CURRENT_BINARY_DIR}/stubs C_VISIBILITY_PRESET default)
  target_link_options(
    binder_ndk_stub
    PRIVATE -Wl,--version-script=${STUB}.ver -nostdlib)

  file(GLOB_RECURSE AIDL_SRCS ${AIDL_GEN_DIR}/src/*.cpp)
  add_library(lapk_aidl STATIC ${AIDL_SRCS})
  target_include_directories(lapk_aidl PUBLIC ${HAL_INCLUDES})
  target_compile_options(lapk_aidl PRIVATE -w)
  target_link_libraries(lapk_aidl PUBLIC binder_ndk_stub log)

  add_executable(allocator allocator/main.cpp)
  set_target_properties(
    allocator
    PROPERTIES OUTPUT_NAME android.hardware.graphics.allocator-service.linuxapk)
  target_link_libraries(allocator PRIVATE lapk_common lapk_aidl)

  # libwayland-client 1.24.0 + protocol glue + libffi, all static (see
  # wayland/fetch-deps.sh, which build.sh runs first).
  if(NOT WAYLAND_DEPS_DIR)
    message(FATAL_ERROR "WAYLAND_DEPS_DIR not set (run wayland/fetch-deps.sh)")
  endif()
  set(WL_SRC ${WAYLAND_DEPS_DIR}/src/wayland-1.24.0/src)
  file(GLOB WL_GEN_SRCS ${WAYLAND_DEPS_DIR}/gen/src/*.c)
  add_library(
    lapk_wayland
    STATIC
      ${WL_SRC}/wayland-client.c ${WL_SRC}/connection.c ${WL_SRC}/wayland-os.c
      ${WL_SRC}/wayland-util.c ${WL_GEN_SRCS})
  target_include_directories(
    lapk_wayland
    PUBLIC ${WAYLAND_DEPS_DIR}/gen/include ${WL_SRC}
    PRIVATE ${WAYLAND_DEPS_DIR}/ffi/include)
  target_compile_options(lapk_wayland PRIVATE -w -fvisibility=hidden)
  target_link_libraries(
    lapk_wayland
    PUBLIC ${WAYLAND_DEPS_DIR}/ffi/lib/libffi.a)

  add_executable(
    composer
    composer/WaylandPresenter.cpp
    composer/InputServer.cpp
    composer/main.cpp
    composer/Composer.cpp
    composer/ComposerClient.cpp
    composer/Display.cpp
    composer/VsyncThread.cpp
    composer/Buffer.cpp
    composer/HeadlessPresenter.cpp
    composer/FrameDumper.cpp
    composer/Fence.cpp
    composer/Properties.cpp)
  set_target_properties(
    composer
    PROPERTIES OUTPUT_NAME android.hardware.graphics.composer3-service.linuxapk)
  target_include_directories(composer PRIVATE ${HAL}/composer)
  # libz is not LL-NDK: link the NDK's static copy.
  target_link_libraries(
    composer
    PRIVATE lapk_common lapk_aidl lapk_wayland -l:libz.a)
  target_compile_definitions(composer PRIVATE LINUXAPK_HWC_WAYLAND)

  # On-device tests (not installed into /vendor/bin/hw).
  add_executable(gralloc_test tests/android/gralloc_test.cpp)
  target_link_libraries(
    gralloc_test
    PRIVATE lapk_common lapk_aidl dl nativewindow)
  add_executable(hwc_test tests/android/hwc_test.cpp)
  target_link_libraries(hwc_test PRIVATE lapk_common lapk_aidl)
  add_library(haltest_shim SHARED tests/android/haltest_shim.c)
else()
  add_executable(gralloc_host_test tests/host/gralloc_test.cpp)
  target_link_libraries(gralloc_host_test PRIVATE lapk_common dl)
  add_executable(dump_host_test tests/host/dump_test.cpp
                 composer/FrameDumper.cpp composer/Properties.cpp)
  target_include_directories(dump_host_test PRIVATE ${HAL}/composer)
  target_link_libraries(dump_host_test PRIVATE lapk_common z)
  enable_testing()
  add_test(NAME gralloc COMMAND gralloc_host_test $<TARGET_FILE:mapper>)
  add_test(
    NAME gralloc_no_udmabuf
    COMMAND gralloc_host_test $<TARGET_FILE:mapper>)
  set_tests_properties(
    gralloc_no_udmabuf
    PROPERTIES ENVIRONMENT LINUXAPK_UDMABUF=/nonexistent)
  add_test(
    NAME dump
    COMMAND dump_host_test ${CMAKE_CURRENT_BINARY_DIR}/dump-test)
endif()
