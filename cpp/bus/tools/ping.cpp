// myos-bus-ping-cpp [MESSAGE]: the C++ client's Phase 2 check.
// Exits 0 and prints `pong: "<message>" from busd X in N ms` on success.

#include <chrono>
#include <cstdio>
#include <iomanip>
#include <iostream>
#include <sstream>

#include "myos/bus.hpp"

int main(int argc, char** argv) {
  std::string message = "hello from C++";
  if (argc > 1) {
    message = argv[1];
    for (int i = 2; i < argc; ++i) message += std::string(" ") + argv[i];
  }
  try {
    auto bus = myos::bus::Client::connect();
    auto t0 = std::chrono::steady_clock::now();
    auto pong = bus.ping(message);
    double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
    std::ostringstream out;
    out << "pong: " << std::quoted(pong.message()) << " from " << pong.broker_version() << " in " << std::fixed
        << std::setprecision(2) << ms << " ms";
    std::cout << out.str() << std::endl;
    return 0;
  } catch (const myos::bus::Error& e) {
    std::cerr << "myos-bus-ping-cpp: " << e.what() << std::endl;
    return 1;
  }
}
