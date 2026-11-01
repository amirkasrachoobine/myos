// What ps and top share: fetching from sysinfod over the bus, and the
// process columns they can show.

#pragma once

#include <optional>
#include <string>
#include <vector>

#include "myos/bus.hpp"
#include "myos/sysinfo/v1/sysinfo.pb.h"

namespace myos::util {

namespace si = ::myos::sysinfo::v1;

struct Snapshot {
  si::SystemReply system;
  si::CpuReply cpu;
  si::MemoryReply memory;
  si::ProcessesReply processes;
};

// One connection to sysinfod; reconnects after the bus or sysinfod restarts.
class SysinfoClient {
 public:
  // Throws myos::bus::Error.
  si::ProcessesReply processes();
  si::SystemReply system();
  Snapshot snapshot();

 private:
  template <class Reply, class Request>
  Reply call(const char* method, const Request& req);
  std::optional<myos::bus::Client> bus_;
};

// "can't reach sysinfod over the bus: ... (is it running? try `initctl status sysinfo`)"
std::string explain(const myos::bus::Error& e);

// A column ps -o and top can show.
struct Field {
  const char* key;     // -o name
  const char* header;
  bool right;          // right-aligned (numbers)
  std::string (*get)(const si::Process& p, const Snapshot& s);
  // For sorting: a number, or the text if `numeric` is false.
  double (*num)(const si::Process& p, const Snapshot& s);
};

const std::vector<Field>& fields();
const Field* find_field(const std::string& key);  // also accepts aliases like pcpu, cmd, vsize

// [dd-]hh:mm:ss, as ps shows CPU time and elapsed time.
std::string clock_time(unsigned long long seconds, bool always_hours);
std::string command_text(const si::Process& p, bool full);  // "[kthreadd]" for kernel threads

}  // namespace myos::util
