# libmyosbus: C++ client for the myos IPC bus, plus myos-bus-ping-cpp.
#
# The .proto files are shared with the other languages and live at the top
# of the repo. Buildroot builds from a copy of this directory only, so it
# passes -DMYOS_PROTO_DIR and -DPROTOC (the host protoc); a plain
#   cmake -S cpp/bus -B /tmp/b && cmake --build /tmp/b
# finds them at ../../proto and protoc on PATH.

cmake_minimum_required(VERSION 3.16)
project(myos-bus-cpp VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

set(MYOS_PROTO_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../proto" CACHE PATH "Directory holding myos/*/v1/*.proto")
find_program(PROTOC protoc REQUIRED)
find_package(PkgConfig REQUIRED)
pkg_check_modules(PROTOBUF REQUIRED IMPORTED_TARGET protobuf)

set(GEN "${CMAKE_CURRENT_BINARY_DIR}/gen")
file(MAKE_DIRECTORY "${GEN}")
set(PB_SRCS)
set(PB_HDRS)
# The bus envelope, plus the Phase 3 services' interfaces, so C++ programs
# can call them (Phase 4's ps and top use sysinfo).
foreach(p bus/v1/wire bus/v1/bus sysinfo/v1/sysinfo log/v1/log net/v1/net)
  set(src "${MYOS_PROTO_DIR}/myos/${p}.proto")
  if(NOT EXISTS "${src}")
    message(FATAL_ERROR "${src} not found; set -DMYOS_PROTO_DIR to the repo's proto/ directory")
  endif()
  set(out "${GEN}/myos/${p}.pb")
  add_custom_command(
    OUTPUT "${out}.cc" "${out}.h"
    COMMAND "${PROTOC}" "--cpp_out=${GEN}" "-I${MYOS_PROTO_DIR}" "${src}"
    DEPENDS "${src}"
    COMMENT "protoc ${p}.proto")
  list(APPEND PB_SRCS "${out}.cc")
  list(APPEND PB_HDRS "${out}.h")
endforeach()

add_library(myosbus STATIC src/bus.cpp ${PB_SRCS})
target_include_directories(myosbus PUBLIC
  $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
  $<BUILD_INTERFACE:${GEN}>
  $<INSTALL_INTERFACE:include>)
target_link_libraries(myosbus PUBLIC PkgConfig::PROTOBUF)
set_target_properties(myosbus PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_compile_options(myosbus PRIVATE -Wall -Wextra)

add_executable(myos-bus-ping-cpp tools/ping.cpp)
target_link_libraries(myos-bus-ping-cpp PRIVATE myosbus)
target_compile_options(myos-bus-ping-cpp PRIVATE -Wall -Wextra)

# Installed for the Phase 4 utilities (ps, top) to build against.
include(GNUInstallDirs)
install(TARGETS myos-bus-ping-cpp RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(TARGETS myosbus ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR})
install(DIRECTORY include/ DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
install(DIRECTORY "${GEN}/myos/" DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}/myos FILES_MATCHING PATTERN "*.pb.h")
configure_file(myos-bus.pc.in myos-bus.pc @ONLY)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/myos-bus.pc" DESTINATION ${CMAKE_INSTALL_LIBDIR}/pkgconfig)
