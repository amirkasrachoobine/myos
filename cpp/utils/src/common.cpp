#include "common.hpp"

#include <grp.h>
#include <pwd.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <map>

namespace myos::util {

int term_width(int fd) {
  winsize ws{};
  if (isatty(fd) && ioctl(fd, TIOCGWINSZ, &ws) == 0 && ws.ws_col > 0) return ws.ws_col;
  if (const char* c = std::getenv("COLUMNS")) {
    int v = std::atoi(c);
    if (v > 0) return v;
  }
  return isatty(fd) ? 80 : 0;
}

int term_height(int fd) {
  winsize ws{};
  if (isatty(fd) && ioctl(fd, TIOCGWINSZ, &ws) == 0 && ws.ws_row > 0) return ws.ws_row;
  if (const char* c = std::getenv("LINES")) {
    int v = std::atoi(c);
    if (v > 0) return v;
  }
  return 24;
}

const std::string& user_name(uid_t uid) {
  static std::map<uid_t, std::string> cache;
  auto it = cache.find(uid);
  if (it != cache.end()) return it->second;
  const passwd* pw = getpwuid(uid);
  return cache[uid] = pw ? pw->pw_name : std::to_string(uid);
}

const std::string& group_name(gid_t gid) {
  static std::map<gid_t, std::string> cache;
  auto it = cache.find(gid);
  if (it != cache.end()) return it->second;
  const group* gr = getgrgid(gid);
  return cache[gid] = gr ? gr->gr_name : std::to_string(gid);
}

bool lookup_user(const std::string& name, uid_t& uid) {
  if (const passwd* pw = getpwnam(name.c_str())) {
    uid = pw->pw_uid;
    return true;
  }
  char* end = nullptr;
  long v = std::strtol(name.c_str(), &end, 10);
  if (!name.empty() && end && *end == '\0' && v >= 0) {
    uid = static_cast<uid_t>(v);
    return true;
  }
  return false;
}

std::string human_size(uint64_t bytes) {
  static const char units[] = "KMGTPE";
  if (bytes < 1024) return std::to_string(bytes);
  double v = static_cast<double>(bytes);
  int u = -1;
  while (v >= 1024 && u < 5) {
    v /= 1024;
    ++u;
  }
  char buf[32];
  // Like GNU ls: one decimal below 10, rounded up.
  if (v < 10) {
    double r = static_cast<long long>(v * 10 + 0.999999) / 10.0;
    if (r >= 10) {
      std::snprintf(buf, sizeof buf, "%.0f%c", r, units[u]);
    } else {
      std::snprintf(buf, sizeof buf, "%.1f%c", r, units[u]);
    }
  } else {
    double r = static_cast<long long>(v + 0.999999);
    if (r >= 1024 && u < 5) {
      std::snprintf(buf, sizeof buf, "1.0%c", units[u + 1]);
    } else {
      std::snprintf(buf, sizeof buf, "%.0f%c", r, units[u]);
    }
  }
  return buf;
}

int utf8_width(const std::string& s) {
  int w = 0;
  for (unsigned char c : s) {
    if ((c & 0xC0) != 0x80) ++w;
  }
  return w;
}

std::vector<std::string> Table::render(int gap) const {
  std::vector<int> width;
  for (const auto& r : rows) {
    if (width.size() < r.size()) width.resize(r.size(), 0);
    for (size_t i = 0; i < r.size(); ++i) width[i] = std::max(width[i], utf8_width(r[i]));
  }
  std::vector<std::string> out;
  for (const auto& r : rows) {
    std::string line;
    for (size_t i = 0; i < r.size(); ++i) {
      int pad = width[i] - utf8_width(r[i]);
      bool last = i + 1 == r.size();
      bool rj = i < right.size() && right[i];
      if (rj) line.append(pad, ' ');
      line += r[i];
      if (!rj && !last) line.append(pad, ' ');
      if (!last) line.append(gap, ' ');
    }
    out.push_back(line);
  }
  return out;
}

std::vector<std::string> split(const std::string& s, char sep) {
  std::vector<std::string> out;
  size_t b = 0;
  for (;;) {
    size_t e = s.find(sep, b);
    out.push_back(s.substr(b, e == std::string::npos ? std::string::npos : e - b));
    if (e == std::string::npos) break;
    b = e + 1;
  }
  return out;
}

}  // namespace myos::util
