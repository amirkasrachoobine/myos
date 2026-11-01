// cat: concatenate files to standard output.

#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <unistd.h>

#include <cstdio>
#include <cstring>
#include <iostream>
#include <string>

#include "common.hpp"

namespace {

const char* kUsage =
    "usage: cat [-nbsEeTtvAu] [FILE...]    (no FILE, or -: standard input)\n"
    "  -n  number all lines          -b  number non-blank lines\n"
    "  -s  squeeze repeated blank lines\n"
    "  -E  show $ at line ends       -T  show tabs as ^I\n"
    "  -v  show control characters as ^X and M-X\n"
    "  -A  same as -vET   -e  same as -vE   -t  same as -vT   -u  (ignored)\n";

bool number = false, number_nonblank = false, squeeze = false, show_ends = false, show_tabs = false,
     show_nonprinting = false;

// Line state carries across files, like GNU cat.
long line_no = 0;
bool at_line_start = true;
int blank_run = 0;

bool write_all(const char* p, size_t n) {
  while (n) {
    ssize_t w = write(1, p, n);
    if (w < 0) {
      if (errno == EINTR) continue;
      return false;
    }
    p += w;
    n -= static_cast<size_t>(w);
  }
  return true;
}

bool plain_copy(int fd, const char* name) {
  static char buf[65536];
  for (;;) {
    ssize_t n = read(fd, buf, sizeof buf);
    if (n < 0) {
      if (errno == EINTR) continue;
      std::fprintf(stderr, "cat: %s: %s\n", name, std::strerror(errno));
      return false;
    }
    if (n == 0) return true;
    if (!write_all(buf, static_cast<size_t>(n))) {
      if (errno != EPIPE) std::fprintf(stderr, "cat: write error: %s\n", std::strerror(errno));
      std::exit(1);
    }
  }
}

bool filtered_copy(int fd, const char* name) {
  static char buf[65536];
  std::string out;
  for (;;) {
    ssize_t n = read(fd, buf, sizeof buf);
    if (n < 0) {
      if (errno == EINTR) continue;
      std::fprintf(stderr, "cat: %s: %s\n", name, std::strerror(errno));
      return false;
    }
    if (n == 0) break;
    out.clear();
    for (ssize_t i = 0; i < n; ++i) {
      unsigned char c = static_cast<unsigned char>(buf[i]);
      if (at_line_start) {
        bool blank = c == '\n';
        if (blank) {
          if (squeeze && blank_run >= 1) continue;
          ++blank_run;
        } else {
          blank_run = 0;
        }
        if (number_nonblank ? !blank : number) {
          char num[32];
          std::snprintf(num, sizeof num, "%6ld\t", ++line_no);
          out += num;
        }
        at_line_start = false;
      }
      if (c == '\n') {
        if (show_ends) out += '$';
        out += '\n';
        at_line_start = true;
        continue;
      }
      if (c == '\t') {
        out += show_tabs ? "^I" : "\t";
        continue;
      }
      if (show_nonprinting) {
        if (c >= 128) {
          out += "M-";
          c -= 128;
        }
        if (c < 32) {
          out += '^';
          out += static_cast<char>(c + 64);
        } else if (c == 127) {
          out += "^?";
        } else {
          out += static_cast<char>(c);
        }
        continue;
      }
      out += static_cast<char>(c);
    }
    if (!write_all(out.data(), out.size())) {
      if (errno != EPIPE) std::fprintf(stderr, "cat: write error: %s\n", std::strerror(errno));
      std::exit(1);
    }
  }
  return true;
}

}  // namespace

int main(int argc, char** argv) {
  static const option longopts[] = {{"number", no_argument, nullptr, 'n'},
                                    {"number-nonblank", no_argument, nullptr, 'b'},
                                    {"squeeze-blank", no_argument, nullptr, 's'},
                                    {"show-ends", no_argument, nullptr, 'E'},
                                    {"show-tabs", no_argument, nullptr, 'T'},
                                    {"show-nonprinting", no_argument, nullptr, 'v'},
                                    {"show-all", no_argument, nullptr, 'A'},
                                    {"help", no_argument, nullptr, 'H'},
                                    {"version", no_argument, nullptr, 'V'},
                                    {nullptr, 0, nullptr, 0}};
  int c;
  while ((c = getopt_long(argc, argv, "nbsEeTtvAu", longopts, nullptr)) != -1) {
    switch (c) {
      case 'n': number = true; break;
      case 'b': number_nonblank = true; break;
      case 's': squeeze = true; break;
      case 'E': show_ends = true; break;
      case 'T': show_tabs = true; break;
      case 'v': show_nonprinting = true; break;
      case 'A': show_nonprinting = show_ends = show_tabs = true; break;
      case 'e': show_nonprinting = show_ends = true; break;
      case 't': show_nonprinting = show_tabs = true; break;
      case 'u': break;
      case 'H': std::cout << kUsage; return 0;
      case 'V': std::cout << "cat (myos) " MYOS_UTILS_VERSION "\n"; return 0;
      default: std::cerr << kUsage; return 2;
    }
  }
  bool filtered = number || number_nonblank || squeeze || show_ends || show_tabs || show_nonprinting;
  int status = 0;
  auto one = [&](const char* name) {
    bool is_stdin = std::strcmp(name, "-") == 0;
    int fd = is_stdin ? 0 : open(name, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
      std::fprintf(stderr, "cat: %s: %s\n", name, std::strerror(errno));
      status = 1;
      return;
    }
    bool ok = filtered ? filtered_copy(fd, name) : plain_copy(fd, name);
    if (!ok) status = 1;
    if (!is_stdin) close(fd);
  };
  if (optind >= argc) {
    one("-");
  } else {
    for (int i = optind; i < argc; ++i) one(argv[i]);
  }
  return status;
}
