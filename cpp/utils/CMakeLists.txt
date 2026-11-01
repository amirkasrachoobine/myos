# The myos utilities: ls, cat, ps and top.
#
# ps and top get their data from sysinfod over the bus, through libmyosbus.
# Buildroot builds cpp/bus first and finds it with pkg-config (myos-bus.pc
# in staging). On your own machine, building this directory on its own
# builds ../bus along with it:
#   cmake -S cpp/utils -B /tmp/utils && cmake --build /tmp/utils

cmake_minimum_required(VERSION 3.16)
project(myos-utils VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(PkgConfig REQUIRED)
pkg_check_modules(MYOSBUS IMPORTED_TARGET myos-bus)
if(MYOSBUS_FOUND)
  set(BUS_LIB PkgConfig::MYOSBUS)
elseif(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/../bus/CMakeLists.txt")
  add_subdirectory("${CMAKE_CURRENT_SOURCE_DIR}/../bus" bus EXCLUDE_FROM_ALL)
  set(BUS_LIB myosbus)
else()
  message(FATAL_ERROR "libmyosbus not found: install cpp/bus first (pkg-config myos-bus)")
endif()

add_library(utilcommon STATIC src/common.cpp)
target_compile_definitions(utilcommon PUBLIC MYOS_UTILS_VERSION="${PROJECT_VERSION}")

add_library(procs STATIC src/procs.cpp)
target_link_libraries(procs PUBLIC utilcommon ${BUS_LIB})

foreach(tool ls cat)
  add_executable(${tool} src/${tool}.cpp)
  target_link_libraries(${tool} PRIVATE utilcommon)
endforeach()
foreach(tool ps top)
  add_executable(${tool} src/${tool}.cpp)
  target_link_libraries(${tool} PRIVATE procs)
endforeach()
foreach(t utilcommon procs ls cat ps top)
  target_compile_options(${t} PRIVATE -Wall -Wextra)
endforeach()

include(GNUInstallDirs)
install(TARGETS ls cat ps top RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
# Buildroot installs them itself instead (build/package/myos-utils), over
# BusyBox's links in /bin and /usr/bin.
