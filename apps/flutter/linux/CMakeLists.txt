# Project-level configuration.
cmake_minimum_required(VERSION 3.13)
project(runner LANGUAGES CXX)

# The name of the executable created for the application. Change this to change
# the on-disk name of your application.
set(BINARY_NAME "waddle_flutter")
# The unique GTK application identifier for this application. See:
# https://wiki.gnome.org/HowDoI/ChooseApplicationID
set(APPLICATION_ID "social.waddle.flutter")

# Explicitly opt in to modern CMake behaviors to avoid warnings with recent
# versions of CMake.
cmake_policy(SET CMP0063 NEW)

# Load bundled libraries from the lib/ directory relative to the binary.
set(CMAKE_INSTALL_RPATH "$ORIGIN/lib")

# Root filesystem for cross-building.
if(FLUTTER_TARGET_PLATFORM_SYSROOT)
  set(CMAKE_SYSROOT ${FLUTTER_TARGET_PLATFORM_SYSROOT})
  set(CMAKE_FIND_ROOT_PATH ${CMAKE_SYSROOT})
  set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
  set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
  set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
  set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
endif()

# Define build configuration options.
if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
  set(CMAKE_BUILD_TYPE "Debug" CACHE
    STRING "Flutter build mode" FORCE)
  set_property(CACHE CMAKE_BUILD_TYPE PROPERTY STRINGS
    "Debug" "Profile" "Release")
endif()

# Compilation settings that should be applied to most targets.
#
# Be cautious about adding new options here, as plugins use this function by
# default. In most cases, you should add new options to specific targets instead
# of modifying this function.
function(APPLY_STANDARD_SETTINGS TARGET)
  target_compile_features(${TARGET} PUBLIC cxx_std_14)
  target_compile_options(${TARGET} PRIVATE -Wall -Werror)
  target_compile_options(${TARGET} PRIVATE "$<$<NOT:$<CONFIG:Debug>>:-O3>")
  target_compile_definitions(${TARGET} PRIVATE "$<$<NOT:$<CONFIG:Debug>>:NDEBUG>")
endfunction()

# Flutter library and tool build rules.
set(FLUTTER_MANAGED_DIR "${CMAKE_CURRENT_SOURCE_DIR}/flutter")
add_subdirectory(${FLUTTER_MANAGED_DIR})

# System-level dependencies.
find_package(PkgConfig REQUIRED)
pkg_check_modules(GTK REQUIRED IMPORTED_TARGET gtk+-3.0)

# Application build; see runner/CMakeLists.txt.
add_subdirectory("runner")

# Run the Flutter tool portions of the build. This must not be removed.
add_dependencies(${BINARY_NAME} flutter_assemble)

# Cargo tracks the source and generated dispatch dependencies of the workspace.
get_filename_component(SERVER_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../server" ABSOLUTE)
if(DEFINED ENV{CARGO_TARGET_DIR})
  get_filename_component(RUST_TARGET_DIR "$ENV{CARGO_TARGET_DIR}" ABSOLUTE BASE_DIR "${SERVER_DIR}")
else()
  set(RUST_TARGET_DIR "${SERVER_DIR}/target")
endif()
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(RUST_PROFILE dev)
  set(RUST_OUTPUT_DIR debug)
else()
  set(RUST_PROFILE release)
  set(RUST_OUTPUT_DIR release)
endif()
set(RUST_LIBRARY "${RUST_TARGET_DIR}/${RUST_OUTPUT_DIR}/libwaddle_flutter_runtime.so")
add_custom_target(waddle_rust_runtime
  COMMAND cargo build --locked --package waddle-flutter-runtime
    --profile ${RUST_PROFILE} --target-dir "${RUST_TARGET_DIR}"
  WORKING_DIRECTORY "${SERVER_DIR}"
  VERBATIM)
add_dependencies(${BINARY_NAME} waddle_rust_runtime)

# Only the install-generated bundle's copy of the executable will launch
# correctly, since the resources must in the right relative locations. To avoid
# people trying to run the unbundled copy, put it in a subdirectory instead of
# the default top-level location.
set_target_properties(${BINARY_NAME}
  PROPERTIES
  RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/intermediates_do_not_run"
)


# Generated plugin build rules, which manage building the plugins and adding
# them to the application.
# The media plugin can use the platform allocator. Avoid downloading a second
# allocator during CMake configuration.
set(MIMALLOC_USE_STATIC_LIBS OFF CACHE BOOL "" FORCE)
include(flutter/generated_plugins.cmake)
pkg_check_modules(VIDEO_PLATFORM REQUIRED IMPORTED_TARGET egl wayland-client)
target_link_libraries(media_kit_video_plugin PRIVATE PkgConfig::VIDEO_PLATFORM)

# The prebuilt WebRTC engine has no runpath of its own and loads PipeWire
# dynamically for Wayland screen capture.
pkg_check_modules(WEBRTC_RUNTIME REQUIRED gbm libdrm libpipewire-0.3)
target_link_libraries(flutter_webrtc_plugin PRIVATE
  "-Wl,--no-as-needed" ${WEBRTC_RUNTIME_LINK_LIBRARIES} "-Wl,--as-needed")


# === Installation ===
# By default, "installing" just makes a relocatable bundle in the build
# directory.
set(BUILD_BUNDLE_DIR "${PROJECT_BINARY_DIR}/bundle")
if(CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT)
  set(CMAKE_INSTALL_PREFIX "${BUILD_BUNDLE_DIR}" CACHE PATH "..." FORCE)
endif()

# Start with a clean build bundle directory every time.
install(CODE "
  file(REMOVE_RECURSE \"${BUILD_BUNDLE_DIR}/\")
  " COMPONENT Runtime)

set(INSTALL_BUNDLE_DATA_DIR "${CMAKE_INSTALL_PREFIX}/data")
set(INSTALL_BUNDLE_LIB_DIR "${CMAKE_INSTALL_PREFIX}/lib")

install(TARGETS ${BINARY_NAME} RUNTIME DESTINATION "${CMAKE_INSTALL_PREFIX}"
  COMPONENT Runtime)

# The story recorder pipes camera frames and microphone audio into FFmpeg.
if(DEFINED ENV{WADDLE_FFMPEG})
  set(FFMPEG_EXECUTABLE "$ENV{WADDLE_FFMPEG}")
else()
  find_program(FFMPEG_EXECUTABLE NAMES ffmpeg)
endif()
if(NOT FFMPEG_EXECUTABLE)
  message(FATAL_ERROR "FFmpeg with libx264 and PulseAudio is required for recording stories")
endif()
install(PROGRAMS "${FFMPEG_EXECUTABLE}" DESTINATION "${CMAKE_INSTALL_PREFIX}/libexec"
  RENAME ffmpeg COMPONENT Runtime)

install(FILES "${FLUTTER_ICU_DATA_FILE}" DESTINATION "${INSTALL_BUNDLE_DATA_DIR}"
  COMPONENT Runtime)

install(FILES "${FLUTTER_LIBRARY}" "${RUST_LIBRARY}" DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
  COMPONENT Runtime)

foreach(bundled_library ${PLUGIN_BUNDLED_LIBRARIES})
  install(FILES "${bundled_library}"
    DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
    COMPONENT Runtime)
endforeach(bundled_library)

# Copy the native assets provided by the build.dart from all packages.
set(NATIVE_ASSETS_DIR "${PROJECT_BUILD_DIR}native_assets/linux/")
install(DIRECTORY "${NATIVE_ASSETS_DIR}"
   DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
   COMPONENT Runtime)

# Fully re-copy the assets directory on each build to avoid having stale files
# from a previous install.
set(FLUTTER_ASSET_DIR_NAME "flutter_assets")
install(CODE "
  file(REMOVE_RECURSE \"${INSTALL_BUNDLE_DATA_DIR}/${FLUTTER_ASSET_DIR_NAME}\")
  " COMPONENT Runtime)
install(DIRECTORY "${PROJECT_BUILD_DIR}/${FLUTTER_ASSET_DIR_NAME}"
  DESTINATION "${INSTALL_BUNDLE_DATA_DIR}" COMPONENT Runtime)

# Install the AOT library on non-Debug builds only.
if(NOT CMAKE_BUILD_TYPE MATCHES "Debug")
  install(FILES "${AOT_LIBRARY}" DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
    COMPONENT Runtime)
endif()
