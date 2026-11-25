# flutter_webrtc owns the microphone and sender registries. Compile private
# copies of its capture and sender integration points so the pub cache stays immutable.
get_target_property(plugin_source flutter_webrtc_plugin SOURCE_DIR)
set(patch_file "${CMAKE_CURRENT_SOURCE_DIR}/flutter-webrtc-1.6.3-microphone.patch")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${patch_file}")
set(patched_sources flutter_media_stream.cc flutter_peerconnection.cc
  flutter_webrtc.cc flutter_webrtc_base.cc)
foreach(source IN LISTS patched_sources)
  configure_file("${plugin_source}/../common/cpp/src/${source}"
    "${CMAKE_CURRENT_BINARY_DIR}/staging/${source}" COPYONLY)
endforeach()
execute_process(COMMAND patch --batch --fuzz=0 -p1 -i "${patch_file}"
  WORKING_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/staging" RESULT_VARIABLE patch_status)
if(NOT patch_status EQUAL 0)
  message(FATAL_ERROR "Review the pinned flutter_webrtc Linux microphone hooks")
endif()
get_target_property(sources flutter_webrtc_plugin SOURCES)
foreach(source IN LISTS patched_sources)
  list(FILTER sources EXCLUDE REGEX "(^|/)${source}$")
  configure_file("${CMAKE_CURRENT_BINARY_DIR}/staging/${source}"
    "${CMAKE_CURRENT_BINARY_DIR}/${source}" COPYONLY)
  list(APPEND sources "${CMAKE_CURRENT_BINARY_DIR}/${source}")
endforeach()
set_property(TARGET flutter_webrtc_plugin PROPERTY SOURCES "${sources}")
target_sources(flutter_webrtc_plugin PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/microphone_filter.cc")
target_include_directories(flutter_webrtc_plugin PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}")
