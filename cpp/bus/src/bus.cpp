#include "myos/bus.hpp"

#include <poll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <utility>

namespace myos::bus {
namespace {

constexpr uint32_t kMaxFrame = 16u << 20;
// The broker is told the same deadline and normally answers TIMEOUT first.
constexpr std::chrono::milliseconds kGrace{1000};

std::string errno_text(const std::string& what) { return what + ": " + std::strerror(errno); }

}  // namespace

std::string socket_path() {
  const char* env = std::getenv("MYOS_BUS_SOCKET");
  return (env && *env) ? env : kDefaultSocket;
}

Client Client::connect(const std::string& path) {
  sockaddr_un addr{};
  addr.sun_family = AF_UNIX;
  if (path.size() >= sizeof(addr.sun_path)) {
    throw Error(Error::Kind::Io, "socket path too long: " + path);
  }
  std::memcpy(addr.sun_path, path.c_str(), path.size() + 1);
  int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) throw Error(Error::Kind::Io, errno_text("socket"));
  if (::connect(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0) {
    std::string msg = errno_text("can't connect to " + path);
    ::close(fd);
    throw Error(Error::Kind::Io, msg);
  }
  return Client(fd);
}

Client::Client(Client&& other) noexcept
    : fd_(std::exchange(other.fd_, -1)), next_id_(other.next_id_), timeout_(other.timeout_) {}

Client& Client::operator=(Client&& other) noexcept {
  if (this != &other) {
    close_fd();
    fd_ = std::exchange(other.fd_, -1);
    next_id_ = other.next_id_;
    timeout_ = other.timeout_;
  }
  return *this;
}

Client::~Client() { close_fd(); }

void Client::close_fd() {
  if (fd_ >= 0) ::close(fd_);
  fd_ = -1;
}

void Client::set_timeout(std::chrono::milliseconds timeout) {
  timeout_ = timeout.count() > 0 ? timeout : std::chrono::milliseconds(1);
}

void Client::write_frame(const v1::Frame& frame) {
  std::string body = frame.SerializeAsString();
  if (body.size() > kMaxFrame) throw Error(Error::Kind::Protocol, "request too large");
  uint32_t n = static_cast<uint32_t>(body.size());
  std::string buf;
  buf.reserve(4 + body.size());
  buf.push_back(static_cast<char>(n >> 24));
  buf.push_back(static_cast<char>(n >> 16));
  buf.push_back(static_cast<char>(n >> 8));
  buf.push_back(static_cast<char>(n));
  buf += body;
  size_t off = 0;
  while (off < buf.size()) {
    // MSG_NOSIGNAL: a dead broker is an exception, not SIGPIPE.
    ssize_t w = ::send(fd_, buf.data() + off, buf.size() - off, MSG_NOSIGNAL);
    if (w < 0 && errno == EINTR) continue;
    if (w < 0) throw Error(Error::Kind::Io, errno_text("bus connection"));
    off += static_cast<size_t>(w);
  }
}

void Client::read_exact(char* buf, size_t len, std::chrono::steady_clock::time_point deadline) {
  size_t got = 0;
  while (got < len) {
    auto left = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - std::chrono::steady_clock::now());
    if (left.count() <= 0) throw Error(Error::Kind::Timeout, "timed out waiting for a reply");
    pollfd p{fd_, POLLIN, 0};
    int r = ::poll(&p, 1, static_cast<int>(std::min<long long>(left.count(), 1 << 30)));
    if (r < 0 && errno == EINTR) continue;
    if (r < 0) throw Error(Error::Kind::Io, errno_text("poll"));
    if (r == 0) continue;  // loop re-checks the deadline
    ssize_t n = ::recv(fd_, buf + got, len - got, 0);
    if (n < 0 && (errno == EINTR || errno == EAGAIN)) continue;
    if (n < 0) throw Error(Error::Kind::Io, errno_text("bus connection"));
    if (n == 0) throw Error(Error::Kind::Io, "the broker closed the connection");
    got += static_cast<size_t>(n);
  }
}

v1::Frame Client::read_frame(std::chrono::steady_clock::time_point deadline) {
  unsigned char hdr[4];
  read_exact(reinterpret_cast<char*>(hdr), 4, deadline);
  uint32_t n = (uint32_t{hdr[0]} << 24) | (uint32_t{hdr[1]} << 16) | (uint32_t{hdr[2]} << 8) | hdr[3];
  if (n > kMaxFrame) throw Error(Error::Kind::Protocol, "oversized frame from the broker");
  std::string body(n, '\0');
  read_exact(body.data(), n, deadline);
  v1::Frame frame;
  if (!frame.ParseFromString(body)) throw Error(Error::Kind::Protocol, "undecodable frame from the broker");
  return frame;
}

std::string Client::call_raw(std::string_view service, std::string_view method, const std::string& payload) {
  if (fd_ < 0) throw Error(Error::Kind::Io, "connection closed after an earlier error");
  const uint64_t id = ++next_id_;
  v1::Frame frame;
  frame.set_id(id);
  v1::Call* call = frame.mutable_call();
  call->set_service(std::string(service));
  call->set_method(std::string(method));
  call->set_payload(payload);
  call->set_timeout_ms(static_cast<uint32_t>(std::min<long long>(timeout_.count(), UINT32_MAX)));

  try {
    write_frame(frame);
    auto deadline = std::chrono::steady_clock::now() + timeout_ + kGrace;
    for (;;) {
      v1::Frame in = read_frame(deadline);
      if (in.id() != id) continue;  // not ours; can't normally happen
      if (!in.has_reply()) throw Error(Error::Kind::Protocol, "expected a reply");
      const v1::Reply& reply = in.reply();
      if (reply.status().code() != v1::Status::OK) {
        std::string what = v1::Status_Code_Name(reply.status().code());
        if (!reply.status().message().empty()) what += ": " + reply.status().message();
        throw Error(Error::Kind::Status, what, reply.status().code());
      }
      return reply.payload();
    }
  } catch (const Error& e) {
    // After a broken or timed-out exchange the stream can't be trusted.
    if (e.kind() != Error::Kind::Status) close_fd();
    throw;
  }
}

v1::PingReply Client::ping(std::string_view message) {
  v1::PingRequest req;
  req.set_message(std::string(message));
  return call<v1::PingReply>("bus", "Ping", req);
}

std::vector<v1::ServiceInfo> Client::list_services() {
  auto reply = call<v1::ListServicesReply>("bus", "ListServices", v1::ListServicesRequest{});
  return {reply.services().begin(), reply.services().end()};
}

}  // namespace myos::bus
