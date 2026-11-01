// Small helpers shared by the myos utilities.

#pragma once

#include <sys/types.h>

#include <cstdint>
#include <string>
#include <vector>

namespace myos::util {

#ifndef MYOS_UTILS_VERSION
#define MYOS_UTILS_VERSION "0.1.0"
#endif

// Terminal width of `fd`: the kernel's idea, else $COLUMNS, else 0 (not a terminal).
int term_width(int fd);
int term_height(int fd);

// Names for ids, cached; the number as text if there's no entry.
const std::string& user_name(uid_t uid);
const std::string& group_name(gid_t gid);
bool lookup_user(const std::string& name, uid_t& uid);

// 1023 -> "1023", 1536 -> "1.5K", 10 MiB -> "10M" (ls -h style).
std::string human_size(uint64_t bytes);

// Columns of numbers and words, right- or left-aligned, widths from the data.
struct Table {
  std::vector<bool> right;  // per column
  std::vector<std::vector<std::string>> rows;
  // Joins columns with `gap` spaces; the last column is never padded.
  std::vector<std::string> render(int gap = 1) const;
};

std::vector<std::string> split(const std::string& s, char sep);
int utf8_width(const std::string& s);

}  // namespace myos::util
