// C++ client for the myos IPC bus.
//
//   #include <myos/bus.hpp>
//
//   auto bus = myos::bus::Client::connect();
//   myos::bus::v1::PingReply pong = bus.ping("hello");
//
//   // Any service's method, with its generated request/reply types:
//   auto info = bus.call<sysinfo::v1::MemoryReply>("sysinfo", "Memory", sysinfo::v1::MemoryRequest{});
//
// Failures throw myos::bus::Error. The wire format is described in
// proto/myos/bus/v1/wire.proto.

#pragma once

#include <chrono>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

#include "myos/bus/v1/bus.pb.h"
#include "myos/bus/v1/wire.pb.h"

namespace myos::bus {

namespace v1 = ::myos::bus::v1;
using Code = v1::Status_Code;

inline constexpr const char* kDefaultSocket = "/run/myos/bus.sock";

// $MYOS_BUS_SOCKET, or /run/myos/bus.sock.
std::string socket_path();

class Error : public std::runtime_error {
 public:
  enum class Kind {
    Io,        // couldn't reach the broker, or the connection broke
    Status,    // the bus or the service answered with an error; see code()
    Timeout,   // no reply in time
    Protocol,  // the peer broke the wire format
  };
  Error(Kind kind, const std::string& what, Code code = v1::Status::OK)
      : std::runtime_error(what), kind_(kind), code_(code) {}
  Kind kind() const { return kind_; }
  // For Kind::Status: why the call failed. OK otherwise.
  Code code() const { return code_; }

 private:
  Kind kind_;
  Code code_;
};

// One connection to the bus. One call at a time; open several clients for
// concurrent calls. Movable, not copyable.
class Client {
 public:
  static Client connect(const std::string& path = socket_path());

  Client(Client&& other) noexcept;
  Client& operator=(Client&& other) noexcept;
  Client(const Client&) = delete;
  Client& operator=(const Client&) = delete;
  ~Client();

  // Per-call timeout; default 5 s.
  void set_timeout(std::chrono::milliseconds timeout);

  // Calls service.method with a serialized request; returns the serialized reply.
  std::string call_raw(std::string_view service, std::string_view method, const std::string& payload);

  template <class Reply, class Request>
  Reply call(std::string_view service, std::string_view method, const Request& request) {
    Reply reply;
    if (!reply.ParseFromString(call_raw(service, method, request.SerializeAsString()))) {
      throw Error(Error::Kind::Protocol, std::string(service) + "." + std::string(method) + " reply didn't decode");
    }
    return reply;
  }

  // Round trip through the broker.
  v1::PingReply ping(std::string_view message);
  // Services currently on the bus.
  std::vector<v1::ServiceInfo> list_services();

 private:
  explicit Client(int fd) : fd_(fd) {}
  void close_fd();
  void write_frame(const v1::Frame& frame);
  v1::Frame read_frame(std::chrono::steady_clock::time_point deadline);
  void read_exact(char* buf, size_t len, std::chrono::steady_clock::time_point deadline);

  int fd_ = -1;
  uint64_t next_id_ = 0;
  std::chrono::milliseconds timeout_{5000};
};

}  // namespace myos::bus
