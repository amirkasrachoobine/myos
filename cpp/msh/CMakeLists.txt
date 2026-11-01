# msh, the myos shell. No dependencies beyond the C++ standard library.
#
#   cmake -S cpp/msh -B /tmp/msh && cmake --build /tmp/msh && /tmp/msh/msh

cmake_minimum_required(VERSION 3.16)
project(msh VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

add_executable(msh
  src/main.cpp src/syntax.cpp src/expand.cpp src/arith.cpp src/exec.cpp
  src/jobs.cpp src/builtins.cpp src/lineedit.cpp src/complete.cpp)
target_compile_options(msh PRIVATE -Wall -Wextra)
target_compile_definitions(msh PRIVATE MSH_VERSION="${PROJECT_VERSION}")

include(GNUInstallDirs)
install(TARGETS msh RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
# /etc/mshrc when the prefix is /usr (as in Buildroot), else <prefix>/etc/mshrc.
install(FILES mshrc DESTINATION ${CMAKE_INSTALL_FULL_SYSCONFDIR})
