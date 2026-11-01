// Tab completion: commands in command position, $variables, and file
// names everywhere else (only directories after `cd`).

#include <dirent.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <cstring>
#include <set>

#include "complete.hpp"

namespace msh {

namespace {

std::string escape(const std::string& s) {
  std::string o;
  for (char c : s) {
    if (std::strchr(" \t\n\\'\"`$&|;<>()*?[]!#{}", c)) o += '\\';
    o += c;
  }
  return o;
}

bool command_word_before(const std::string& before) {
  // `before` is the text before the word being completed, trailing blanks removed.
  if (before.empty()) return true;
  char c = before.back();
  if (std::strchr(";&|(`", c)) return true;
  size_t b = before.find_last_of(" \t\n;&|(`");
  std::string prev = b == std::string::npos ? before : before.substr(b + 1);
  static const std::set<std::string> kw = {"then", "do", "else", "elif", "if", "while", "until", "!", "{",
                                           "time", "exec"};
  if (kw.count(prev)) return true;
  if (is_assignment(prev)) {
    std::string rest = b == std::string::npos ? "" : before.substr(0, b + 1);
    while (!rest.empty() && (rest.back() == ' ' || rest.back() == '\t')) rest.pop_back();
    return command_word_before(rest);
  }
  return false;
}

}  // namespace

Completion complete_line(Shell& sh, const std::string& line, size_t cursor) {
  Completion c;
  // Find where the current word starts, honouring quotes and escapes.
  size_t start = 0, cmd_start = 0;
  char quote = 0;
  for (size_t i = 0; i < cursor; ++i) {
    char ch = line[i];
    if (quote) {
      if (ch == quote) quote = 0;
      else if (ch == '\\' && quote == '"') ++i;
      continue;
    }
    if (ch == '\\') {
      ++i;
    } else if (ch == '\'' || ch == '"') {
      quote = ch;
    } else if (std::strchr(" \t\n;&|<>()`", ch)) {
      start = i + 1;
      if (std::strchr(";&|()`\n", ch)) cmd_start = i + 1;
    }
  }
  if (start > cursor) start = cursor;
  std::string raw = line.substr(start, cursor - start);
  std::string before = line.substr(0, start);
  while (!before.empty() && (before.back() == ' ' || before.back() == '\t')) before.pop_back();
  bool cmd_pos = command_word_before(before);

  std::string cmd = line.substr(cmd_start, start - cmd_start);
  size_t f = cmd.find_first_not_of(" \t");
  cmd = f == std::string::npos ? "" : cmd.substr(f);
  cmd = cmd.substr(0, cmd.find_first_of(" \t"));
  bool dirs_only = !cmd_pos && cmd == "cd";

  // What has been typed, without its quoting.
  char q = (!raw.empty() && (raw[0] == '\'' || raw[0] == '"')) ? raw[0] : 0;
  std::string text;
  for (size_t i = q ? 1 : 0; i < raw.size(); ++i) {
    if (raw[i] == '\\' && q != '\'' && i + 1 < raw.size()) ++i;
    if (raw[i] == q) continue;
    text += raw[i];
  }
  c.start = start;
  std::vector<std::pair<std::string, std::string>> found;  // (replace, display)
  std::vector<char> suffixes;

  if (!q && !text.empty() && text[0] == '$' && text.find('/') == std::string::npos) {
    std::string p = text.substr(text.size() > 1 && text[1] == '{' ? 2 : 1);
    bool braced = text.size() > 1 && text[1] == '{';
    for (auto& [name, v] : sh.vars) {
      if (name.compare(0, p.size(), p) == 0) {
        found.emplace_back((braced ? "${" : "$") + name + (braced ? "}" : ""), name);
        suffixes.push_back(0);
      }
    }
  } else if (cmd_pos && text.find('/') == std::string::npos && !q) {
    std::set<std::string> names;
    for (auto& [n, b] : builtins()) names.insert(n);
    for (auto& [n, v] : sh.aliases) names.insert(n);
    for (auto& [n, v] : sh.functions) names.insert(n);
    for (const char* k : {"if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "exec"})
      names.insert(k);
    std::string path = sh.get_or("PATH");
    size_t b = 0;
    for (;;) {
      size_t e = path.find(':', b);
      std::string dir = path.substr(b, e == std::string::npos ? std::string::npos : e - b);
      if (dir.empty()) dir = ".";
      if (DIR* d = opendir(dir.c_str())) {
        while (dirent* de = readdir(d)) {
          std::string n = de->d_name;
          if (n.compare(0, text.size(), text) != 0 || n == "." || n == "..") continue;
          std::string full = dir + "/" + n;
          struct stat st;
          if (stat(full.c_str(), &st) == 0 && S_ISREG(st.st_mode) && access(full.c_str(), X_OK) == 0) names.insert(n);
        }
        closedir(d);
      }
      if (e == std::string::npos) break;
      b = e + 1;
    }
    for (const std::string& n : names) {
      if (n.compare(0, text.size(), text) == 0) {
        found.emplace_back(escape(n), n);
        suffixes.push_back(' ');
      }
    }
  } else {
    size_t slash = text.rfind('/');
    std::string dir_typed = slash == std::string::npos ? "" : text.substr(0, slash + 1);
    std::string base = slash == std::string::npos ? text : text.substr(slash + 1);
    size_t raw_slash = raw.rfind('/');
    std::string raw_prefix = raw_slash == std::string::npos ? (q ? std::string(1, q) : "") : raw.substr(0, raw_slash + 1);
    std::string dir = dir_typed.empty() ? "." : dir_typed;
    if (dir[0] == '~') {
      size_t s = dir.find('/');
      std::string user = dir.substr(1, s - 1);
      std::string home = user.empty() ? sh.get_or("HOME") : "";
      if (!user.empty()) {
        if (const passwd* pw = getpwnam(user.c_str())) home = pw->pw_dir;
      }
      if (!home.empty()) dir = home + dir.substr(s);
    }
    std::vector<std::pair<std::string, bool>> names;
    if (DIR* d = opendir(dir.c_str())) {
      while (dirent* de = readdir(d)) {
        std::string n = de->d_name;
        if (n == "." || n == "..") continue;
        if (n.compare(0, base.size(), base) != 0) continue;
        if (n[0] == '.' && (base.empty() || base[0] != '.')) continue;
        struct stat st;
        std::string full = dir + "/" + n;
        bool is_dir = stat(full.c_str(), &st) == 0 && S_ISDIR(st.st_mode);
        if (dirs_only && !is_dir) continue;
        if (cmd_pos && !is_dir && access(full.c_str(), X_OK) != 0) continue;
        names.emplace_back(n, is_dir);
      }
      closedir(d);
    }
    std::sort(names.begin(), names.end());
    for (auto& [n, is_dir] : names) {
      found.emplace_back(raw_prefix + (q ? n : escape(n)), n + (is_dir ? "/" : ""));
      suffixes.push_back(is_dir ? '/' : (q ? 0 : ' '));
    }
  }
  for (size_t i = 0; i < found.size(); ++i) {
    c.replace.push_back(found[i].first);
    c.display.push_back(found[i].second);
    c.suffix.push_back(suffixes[i]);
  }
  return c;
}

}  // namespace msh
