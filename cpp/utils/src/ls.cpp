// ls: list directory contents.

#include <dirent.h>
#include <errno.h>
#include <getopt.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <time.h>
#include <unistd.h>

#include <algorithm>
#include <cstring>
#include <iostream>
#include <string>
#include <vector>

#include "common.hpp"

using namespace myos::util;

namespace {

const char* kUsage =
    "usage: ls [-aAdFhilnpRrStU1C] [--color[=auto|always|never]] [FILE...]\n"
    "  -a  all entries, including . and ..   -A  all except . and ..\n"
    "  -l  long format                       -h  sizes like 1.5K 23M (with -l)\n"
    "  -n  numeric user and group ids        -i  inode numbers\n"
    "  -d  list directories themselves       -R  recurse into directories\n"
    "  -t  newest first   -S  largest first  -r  reverse   -U  don't sort\n"
    "  -F  append one of / * @ | =           -p  append / to directories\n"
    "  -1  one per line   -C  columns (the default on a terminal)\n";

struct Opts {
  bool all = false, almost = false, lng = false, human = false, numeric = false, inode = false;
  bool dir_itself = false, recursive = false, by_time = false, by_size = false, reverse = false, unsorted = false;
  bool classify = false, slash = false, one = false, columns = false, color = false, tty = false;
} o;

int status = 0;

struct Entry {
  std::string name;  // as listed
  std::string path;  // to stat / open
  struct stat st {};
  bool ok = false;
  std::string link;  // symlink target
  bool link_dir = false, link_broken = false;
};

std::string printable(const std::string& s) {
  if (!o.tty) return s;
  std::string r;
  for (unsigned char c : s) r += (c < 32 || c == 127) ? '?' : static_cast<char>(c);
  return r;
}

const char* color_of(const Entry& e) {
  mode_t m = e.st.st_mode;
  if (S_ISLNK(m)) return e.link_broken ? "01;31" : "01;36";
  if (S_ISDIR(m)) return (m & S_IWOTH) && (m & S_ISVTX) ? "30;42" : "01;34";
  if (S_ISFIFO(m)) return "33";
  if (S_ISSOCK(m)) return "01;35";
  if (S_ISBLK(m) || S_ISCHR(m)) return "01;33";
  if (m & S_ISUID) return "37;41";
  if (m & S_ISGID) return "30;43";
  if (m & (S_IXUSR | S_IXGRP | S_IXOTH)) return "01;32";
  return nullptr;
}

char type_suffix(const Entry& e) {
  mode_t m = e.st.st_mode;
  if (S_ISDIR(m)) return '/';
  if (o.slash) return 0;
  if (S_ISLNK(m)) return o.lng ? 0 : '@';
  if (S_ISFIFO(m)) return '|';
  if (S_ISSOCK(m)) return '=';
  if (S_ISREG(m) && (m & (S_IXUSR | S_IXGRP | S_IXOTH))) return '*';
  return 0;
}

// The name as shown, colored and with its -F/-p mark; width without escapes.
std::string shown(const Entry& e, int& width) {
  std::string n = printable(e.name);
  width = utf8_width(n);
  std::string s;
  const char* c = o.color ? color_of(e) : nullptr;
  s = c ? std::string("\x1b[") + c + "m" + n + "\x1b[0m" : n;
  if (o.classify || o.slash) {
    char t = type_suffix(e);
    if (t) {
      s += t;
      ++width;
    }
  }
  return s;
}

std::string mode_string(mode_t m) {
  char s[11];
  s[0] = S_ISDIR(m) ? 'd' : S_ISLNK(m) ? 'l' : S_ISCHR(m) ? 'c' : S_ISBLK(m) ? 'b' : S_ISFIFO(m) ? 'p' : S_ISSOCK(m) ? 's' : '-';
  const char* rwx = "rwxrwxrwx";
  for (int i = 0; i < 9; ++i) s[i + 1] = (m & (0400 >> i)) ? rwx[i] : '-';
  if (m & S_ISUID) s[3] = (m & S_IXUSR) ? 's' : 'S';
  if (m & S_ISGID) s[6] = (m & S_IXGRP) ? 's' : 'S';
  if (m & S_ISVTX) s[9] = (m & S_IXOTH) ? 't' : 'T';
  s[10] = 0;
  return s;
}

std::string date_string(time_t t) {
  time_t now = time(nullptr);
  char buf[32];
  struct tm tm {};
  localtime_r(&t, &tm);
  const time_t six_months = 31556952 / 2;
  if (t > now - six_months && t < now + 3600) {
    strftime(buf, sizeof buf, "%b %e %H:%M", &tm);
  } else {
    strftime(buf, sizeof buf, "%b %e  %Y", &tm);
  }
  return buf;
}

void fill(Entry& e) {
  if (lstat(e.path.c_str(), &e.st) != 0) return;
  e.ok = true;
  if (S_ISLNK(e.st.st_mode)) {
    char buf[4096];
    ssize_t n = readlink(e.path.c_str(), buf, sizeof buf - 1);
    if (n >= 0) e.link.assign(buf, n);
    struct stat t {};
    if (stat(e.path.c_str(), &t) == 0) {
      e.link_dir = S_ISDIR(t.st_mode);
    } else {
      e.link_broken = true;
    }
  }
}

void sort_entries(std::vector<Entry>& v) {
  if (o.unsorted) return;
  std::stable_sort(v.begin(), v.end(), [](const Entry& a, const Entry& b) {
    if (o.by_time && a.st.st_mtim.tv_sec != b.st.st_mtim.tv_sec) return a.st.st_mtim.tv_sec > b.st.st_mtim.tv_sec;
    if (o.by_time && a.st.st_mtim.tv_nsec != b.st.st_mtim.tv_nsec) return a.st.st_mtim.tv_nsec > b.st.st_mtim.tv_nsec;
    if (o.by_size && a.st.st_size != b.st.st_size) return a.st.st_size > b.st.st_size;
    return std::strcmp(a.name.c_str(), b.name.c_str()) < 0;
  });
  if (o.reverse) std::reverse(v.begin(), v.end());
}

void print_long(const std::vector<Entry>& v, bool total) {
  if (total) {
    unsigned long long blocks = 0;
    for (const Entry& e : v) blocks += e.st.st_blocks / 2;  // 512-byte blocks to KiB
    std::cout << "total " << (o.human ? human_size(blocks * 1024) : std::to_string(blocks)) << "\n";
  }
  Table t;
  t.right = {true, false, true, false, false, true, false, false};
  if (!o.inode) t.right.erase(t.right.begin());
  for (const Entry& e : v) {
    std::vector<std::string> r;
    if (o.inode) r.push_back(std::to_string(e.st.st_ino));
    r.push_back(mode_string(e.st.st_mode));
    r.push_back(std::to_string(e.st.st_nlink));
    r.push_back(o.numeric ? std::to_string(e.st.st_uid) : user_name(e.st.st_uid));
    r.push_back(o.numeric ? std::to_string(e.st.st_gid) : group_name(e.st.st_gid));
    if (S_ISCHR(e.st.st_mode) || S_ISBLK(e.st.st_mode)) {
      r.push_back(std::to_string(major(e.st.st_rdev)) + ", " + std::to_string(minor(e.st.st_rdev)));
    } else {
      r.push_back(o.human ? human_size(e.st.st_size) : std::to_string(e.st.st_size));
    }
    r.push_back(date_string(e.st.st_mtim.tv_sec));
    int w;
    std::string name = shown(e, w);
    if (S_ISLNK(e.st.st_mode)) name += " -> " + printable(e.link);
    r.push_back(name);
    t.rows.push_back(r);
  }
  for (const std::string& line : t.render()) std::cout << line << "\n";
}

void print_columns(const std::vector<Entry>& v) {
  std::vector<std::string> names;
  std::vector<int> widths;
  for (const Entry& e : v) {
    int w;
    std::string s = shown(e, w);
    if (o.inode) {
      std::string ino = std::to_string(e.st.st_ino) + " ";
      s = ino + s;
      w += static_cast<int>(ino.size());
    }
    names.push_back(s);
    widths.push_back(w);
  }
  if (names.empty()) return;
  int width = term_width(1);
  if (o.columns && width <= 0) width = 80;
  if (o.one || (!o.columns && !o.tty) || width <= 0) {
    for (auto& n : names) std::cout << n << "\n";
    return;
  }
  // The most columns that fit, filled top to bottom like GNU ls.
  size_t n = names.size();
  size_t best_cols = 1;
  std::vector<int> best_w = {*std::max_element(widths.begin(), widths.end())};
  for (size_t cols = std::min<size_t>(n, std::max(1, width / 3)); cols > 1; --cols) {
    size_t rows = (n + cols - 1) / cols;
    if ((cols - 1) * rows >= n) continue;  // would leave an empty column
    std::vector<int> cw(cols, 0);
    for (size_t i = 0; i < n; ++i) cw[i / rows] = std::max(cw[i / rows], widths[i]);
    int total = 0;
    for (size_t c = 0; c < cols; ++c) total += cw[c] + (c + 1 < cols ? 2 : 0);
    if (total <= width) {
      best_cols = cols;
      best_w = cw;
      break;
    }
  }
  size_t rows = (n + best_cols - 1) / best_cols;
  for (size_t r = 0; r < rows; ++r) {
    std::string line;
    for (size_t c = 0; c < best_cols; ++c) {
      size_t i = c * rows + r;
      if (i >= n) break;
      line += names[i];
      if ((c + 1) * rows + r < n) line.append(best_w[c] - widths[i] + 2, ' ');
    }
    std::cout << line << "\n";
  }
}

void print_entries(std::vector<Entry>& v, bool total) {
  sort_entries(v);
  if (o.lng) {
    print_long(v, total);
  } else {
    print_columns(v);
  }
}

void list_dir(const std::string& path, bool header, bool& first) {
  DIR* d = opendir(path.c_str());
  if (!d) {
    std::cerr << "ls: cannot open directory '" << path << "': " << std::strerror(errno) << "\n";
    status = 2;
    return;
  }
  std::vector<Entry> v;
  while (dirent* de = readdir(d)) {
    std::string n = de->d_name;
    if (n[0] == '.') {
      if (!o.all && !o.almost) continue;
      if (o.almost && !o.all && (n == "." || n == "..")) continue;
    }
    Entry e;
    e.name = n;
    e.path = (path == "/" ? "" : path) + "/" + n;
    fill(e);
    if (!e.ok) {
      std::cerr << "ls: cannot access '" << e.path << "': " << std::strerror(errno) << "\n";
      status = 1;
      continue;
    }
    v.push_back(e);
  }
  closedir(d);
  if (!first) std::cout << "\n";
  first = false;
  if (header) std::cout << printable(path) << ":\n";
  print_entries(v, o.lng);
  if (o.recursive) {
    for (const Entry& e : v) {
      if (S_ISDIR(e.st.st_mode) && e.name != "." && e.name != "..") list_dir(e.path, true, first);
    }
  }
}

}  // namespace

int main(int argc, char** argv) {
  o.tty = isatty(1);
  std::string color = "auto";
  static const option longopts[] = {{"color", optional_argument, nullptr, 'c'},
                                    {"colour", optional_argument, nullptr, 'c'},
                                    {"help", no_argument, nullptr, 'H'},
                                    {"version", no_argument, nullptr, 'V'},
                                    {"all", no_argument, nullptr, 'a'},
                                    {"almost-all", no_argument, nullptr, 'A'},
                                    {"human-readable", no_argument, nullptr, 'h'},
                                    {"recursive", no_argument, nullptr, 'R'},
                                    {"reverse", no_argument, nullptr, 'r'},
                                    {"classify", no_argument, nullptr, 'F'},
                                    {"directory", no_argument, nullptr, 'd'},
                                    {nullptr, 0, nullptr, 0}};
  int c;
  while ((c = getopt_long(argc, argv, "aAdFhilnpRrStU1C", longopts, nullptr)) != -1) {
    switch (c) {
      case 'a': o.all = true; break;
      case 'A': o.almost = true; break;
      case 'd': o.dir_itself = true; break;
      case 'F': o.classify = true; break;
      case 'h': o.human = true; break;
      case 'i': o.inode = true; break;
      case 'l': o.lng = true; o.one = false; break;
      case 'n': o.numeric = true; o.lng = true; break;
      case 'p': o.slash = true; break;
      case 'R': o.recursive = true; break;
      case 'r': o.reverse = true; break;
      case 'S': o.by_size = true; o.by_time = false; break;
      case 't': o.by_time = true; o.by_size = false; break;
      case 'U': o.unsorted = true; break;
      case '1': o.one = true; o.lng = false; o.columns = false; break;
      case 'C': o.columns = true; o.one = false; o.lng = false; break;
      case 'c': color = optarg ? optarg : "always"; break;
      case 'H': std::cout << kUsage; return 0;
      case 'V': std::cout << "ls (myos) " MYOS_UTILS_VERSION "\n"; return 0;
      default: std::cerr << kUsage; return 2;
    }
  }
  if (color == "always" || color == "yes" || color == "force") {
    o.color = true;
  } else if (color == "auto" || color == "tty" || color == "if-tty") {
    const char* term = std::getenv("TERM");
    o.color = o.tty && !(term && std::strcmp(term, "dumb") == 0);
  } else if (color != "never" && color != "no" && color != "none") {
    std::cerr << "ls: invalid argument '" << color << "' for '--color'\n" << kUsage;
    return 2;
  }

  std::vector<std::string> args(argv + optind, argv + argc);
  if (args.empty()) args.push_back(".");
  std::vector<Entry> files, dirs;
  for (const std::string& a : args) {
    Entry e;
    e.name = a;
    e.path = a;
    fill(e);
    if (!e.ok) {
      std::cerr << "ls: cannot access '" << a << "': " << std::strerror(errno) << "\n";
      status = 2;
      continue;
    }
    // A symlink to a directory named on the command line is followed, unless -l or -d.
    bool is_dir = S_ISDIR(e.st.st_mode) || (S_ISLNK(e.st.st_mode) && e.link_dir && !o.lng);
    if (is_dir && !o.dir_itself) {
      dirs.push_back(e);
    } else {
      files.push_back(e);
    }
  }
  bool first = true;
  if (!files.empty()) {
    print_entries(files, false);
    first = false;
  }
  sort_entries(dirs);
  bool header = args.size() > 1 || o.recursive;
  for (const Entry& d : dirs) list_dir(d.path, header, first);
  std::cout.flush();
  return status;
}
