# SPDX-License-Identifier: AGPL-3.0-or-later
# Linux.apk display HALs, part of the root superbuild. With the NDK (preset
# "android"): the vendor binaries and on-device tests. Natively (preset
# "host-tests"): host (glibc) unit tests of the buffer/mapper code.
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)
add_compile_options(-Wall -Wextra -Wno-unused-parameter
                    -Wno-missing-field-initializers)

# NDK-backend C++ for the frozen AIDL snapshots, generated at configure time
# with the build-tools pin's aidl.
set(AIDL_GEN_DIR ${CMAKE_CURRENT_BINARY_DIR}/aidl)
file(GLOB_RECURSE AIDL_INPUTS CONFIGURE_DEPENDS third_party/aosp/aidl/*.aidl)
set_property(
  DIRECTORY
  APPEND
  PROPERTY
    CMAKE_CONFIGURE_DEPENDS ${CMAKE_CURRENT_SOURCE_DIR}/tools/gen-aidl.sh
    ${AIDL_INPUTS})
execute_process(
  COMMAND
    ${CMAKE_CURRENT_SOURCE_DIR}/tools/gen-aidl.sh ${AIDL_GEN_DIR} OUTPUT_QUIET
  COMMAND_ERROR_IS_FATAL ANY)

set(HAL ${CMAKE_CURRENT_SOURCE_DIR})
set(HAL_INCLUDES ${HAL}/common/include ${HAL}/third_party/aosp/include
    ${AIDL_GEN_DIR}/include)
if(NOT ANDROID)
  # glibc host: shims for headers that assume Bionic.
  list(APPEND HAL_INCLUDES ${HAL}/common/host-compat)
  add_compile_options(-include ${HAL}/common/host-compat/host_compat.h)
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
      python3 ${HAL}/tools/gen-stub.py
      ${HAL}/third_party/aosp/libbinder_ndk.map.txt ${STUB}
    DEPENDS
      ${HAL}/tools/gen-stub.py ${HAL}/third_party/aosp/libbinder_ndk.map.txt)
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

  add_executable(
    composer
    composer/BridgePresenter.cpp
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
  target_link_libraries(composer PRIVATE lapk_common lapk_aidl -l:libz.a)

  # On-device tests (never installed).
  add_executable(gralloc_test tests/android/gralloc_test.cpp)
  target_link_libraries(
    gralloc_test
    PRIVATE lapk_common lapk_aidl dl nativewindow)
  add_executable(hwc_test tests/android/hwc_test.cpp)
  target_link_libraries(hwc_test PRIVATE lapk_common lapk_aidl)
  # A /vendor layer for tests/bionic/run.sh -v.
  set_target_properties(
    gralloc_test hwc_test
    PROPERTIES
      RUNTIME_OUTPUT_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/test-vendor/bin)

  set(VENDOR ${LINUXAPK_ANDROID}/vendor)
  install(
    TARGETS allocator composer
    RUNTIME DESTINATION ${VENDOR}/bin/hw COMPONENT graphics)
  install(
    TARGETS mapper
    LIBRARY DESTINATION ${VENDOR}/lib64/hw COMPONENT graphics)
  file(GLOB INIT_RCS etc/init/*.rc)
  install(FILES ${INIT_RCS} DESTINATION ${VENDOR}/etc/init COMPONENT graphics)
  file(GLOB VINTF_XMLS etc/vintf/*.xml)
  install(
    FILES ${VINTF_XMLS}
    DESTINATION ${VENDOR}/etc/vintf/manifest
    COMPONENT graphics)
else()
  add_executable(gralloc_host_test tests/host/gralloc_test.cpp)
  target_link_libraries(gralloc_host_test PRIVATE lapk_common dl)
  add_executable(dump_host_test tests/host/dump_test.cpp
                 composer/FrameDumper.cpp composer/Properties.cpp)
  target_include_directories(dump_host_test PRIVATE ${HAL}/composer)
  target_link_libraries(dump_host_test PRIVATE lapk_common z)
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
