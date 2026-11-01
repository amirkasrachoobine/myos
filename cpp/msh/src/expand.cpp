// Word expansion: tilde, parameters, $(...) and `...`, $((...)), then field
// splitting and globbing of the unquoted parts, then quote removal.

#include <fnmatch.h>
#include <glob.h>
#include <pwd.h>
#include <time.h>
#include <unistd.h>

#include <cstring>

#include "shell.hpp"

namespace msh {

namespace {

// Per-character flags: may this character be split on / globbed?
constexpr char kQuoted = 0;   // inside quotes, or escaped
constexpr char kLiteral = 1;  // unquoted source text: globbed, not split
constexpr char kExpanded = 3; // unquoted result of an expansion: split and globbed
constexpr char kGlobBit = 1, kSplitBit = 2;

struct Piece {
  std::string s;
  std::string f;  // one flag per byte of s
  bool exists = false;  // quoted text (even "") makes a field exist
};

std::string home_of(const std::string& user, const Shell& sh) {
  if (user.empty()) {
    auto h = sh.get("HOME");
    if (h) return *h;
    const passwd* pw = getpwuid(getuid());
    return pw ? pw->pw_dir : "/";
  }
  const passwd* pw = getpwnam(user.c_str());
  return pw ? pw->pw_dir : "";
}

class Expander {
 public:
  Expander(Shell& sh, bool split) : sh_(sh), split_(split) { pieces_.emplace_back(); }

  void word(const std::string& raw) {
    size_t i = 0, n = raw.size();
    if (n && raw[0] == '~') {
      size_t end = raw.find('/');
      if (end == std::string::npos) end = n;
      std::string user = raw.substr(1, end - 1);
      if (user.find_first_of("'\"\\$`") == std::string::npos) {
        std::string home = home_of(user, sh_);
        if (!home.empty() || user.empty()) {
          add(home, kQuoted);
          cur().exists = true;
          i = end;
        }
      }
    }
    while (i < n) {
      char c = raw[i];
      if (c == '\\') {
        if (i + 1 < n) add(std::string(1, raw[i + 1]), kQuoted);
        cur().exists = true;
        i += 2;
      } else if (c == '\'') {
        size_t j = raw.find('\'', i + 1);
        if (j == std::string::npos) j = n;
        add(raw.substr(i + 1, j - i - 1), kQuoted);
        cur().exists = true;
        i = j + 1;
      } else if (c == '"') {
        if (raw.compare(i, 4, "\"$@\"") == 0 && sh_.positional.empty()) {
          i += 4;  // "$@" with no arguments is no field at all
          continue;
        }
        cur().exists = true;
        ++i;
        while (i < n && raw[i] != '"') {
          char d = raw[i];
          if (d == '\\' && i + 1 < n && std::strchr("$`\"\\\n", raw[i + 1])) {
            if (raw[i + 1] != '\n') add(std::string(1, raw[i + 1]), kQuoted);
            i += 2;
          } else if (d == '$') {
            i = dollar(raw, i, true);
          } else if (d == '`') {
            i = backtick(raw, i, true);
          } else {
            add(std::string(1, d), kQuoted);
            ++i;
          }
        }
        ++i;
      } else if (c == '$') {
        i = dollar(raw, i, false);
      } else if (c == '`') {
        i = backtick(raw, i, false);
      } else {
        add(std::string(1, c), kLiteral);
        cur().exists = true;
        ++i;
      }
    }
  }

  std::vector<std::string> fields() {
    std::vector<Piece> split;
    for (Piece& p : pieces_) {
      if (split_) {
        split_piece(p, split);
      } else {
        split.push_back(p);
      }
    }
    std::vector<std::string> out;
    for (Piece& p : split) glob_piece(p, out);
    return out;
  }

  std::string joined() {
    std::string s;
    for (size_t i = 0; i < pieces_.size(); ++i) {
      if (i) s += ' ';
      s += pieces_[i].s;
    }
    return s;
  }

 private:
  Piece& cur() { return pieces_.back(); }
  void add(const std::string& s, char flag) {
    cur().s += s;
    cur().f.append(s.size(), flag);
  }
  void field_break() { pieces_.emplace_back(); }

  size_t dollar(const std::string& raw, size_t i, bool dq) {
    size_t n = raw.size();
    char fl = dq ? kQuoted : kExpanded;
    if (i + 1 >= n) {
      add("$", dq ? kQuoted : kLiteral);
      return i + 1;
    }
    char c = raw[i + 1];
    if (c == '(') {
      if (i + 2 < n && raw[i + 2] == '(') {
        size_t end = match_arith(raw, i + 3);
        std::string expr = sh_.expand_string(raw.substr(i + 3, end - i - 3));
        add(std::to_string(sh_.arith(expr)), fl);
        return end + 2;
      }
      size_t end = match_paren(raw, i + 2);
      add(sh_.command_output(raw.substr(i + 2, end - i - 2)), fl);
      return end + 1;
    }
    if (c == '{') {
      size_t end = match_brace(raw, i + 2);
      brace(raw.substr(i + 2, end - i - 2), dq);
      return end + 1;
    }
    if (c == '@' || c == '*') {
      positional(c, dq);
      return i + 2;
    }
    if (std::strchr("?$!#-0123456789", c)) {
      add(special(std::string(1, c)).value_or(""), fl);
      return i + 2;
    }
    if (std::isalpha(static_cast<unsigned char>(c)) || c == '_') {
      size_t j = i + 1;
      while (j < n && (std::isalnum(static_cast<unsigned char>(raw[j])) || raw[j] == '_')) ++j;
      add(sh_.get_or(raw.substr(i + 1, j - i - 1)), fl);
      return j;
    }
    add("$", dq ? kQuoted : kLiteral);
    return i + 1;
  }

  void positional(char c, bool dq) {
    const auto& p = sh_.positional;
    if (dq && c == '@') {
      for (size_t k = 0; k < p.size(); ++k) {
        if (k) field_break();
        cur().exists = true;
        add(p[k], kQuoted);
      }
    } else if (dq) {
      std::string ifs = sh_.get_or("IFS", " ");
      add(join(p, ifs.empty() ? "" : ifs.substr(0, 1)), kQuoted);
    } else {
      for (size_t k = 0; k < p.size(); ++k) {
        if (k) add(" ", kExpanded);
        add(p[k], kExpanded);
      }
    }
  }

  std::optional<std::string> special(const std::string& name) {
    if (name == "?") return std::to_string(sh_.last_status);
    if (name == "$") return std::to_string(sh_.top_pid);
    if (name == "!") return sh_.last_bg ? std::optional<std::string>(std::to_string(sh_.last_bg)) : std::nullopt;
    if (name == "#") return std::to_string(sh_.positional.size());
    if (name == "-") {
      std::string f;
      if (sh_.opt_errexit) f += 'e';
      if (sh_.interactive) f += 'i';
      if (sh_.job_control) f += 'm';
      if (sh_.opt_xtrace) f += 'x';
      return f;
    }
    if (name == "0") return sh_.arg0;
    if (!name.empty() && std::isdigit(static_cast<unsigned char>(name[0]))) {
      size_t k = std::stoul(name);
      if (k >= 1 && k <= sh_.positional.size()) return sh_.positional[k - 1];
      return std::nullopt;
    }
    return sh_.get(name);
  }

  void brace(const std::string& body, bool dq) {
    char fl = dq ? kQuoted : kExpanded;
    if (body.size() > 1 && body[0] == '#') {
      std::string name = body.substr(1);
      if (name == "@" || name == "*") {
        add(std::to_string(sh_.positional.size()), fl);
      } else {
        add(std::to_string(special(name).value_or("").size()), fl);
      }
      return;
    }
    size_t j = 0;
    if (!body.empty() && std::strchr("?$!#-@*", body[0])) {
      j = 1;
    } else if (!body.empty() && std::isdigit(static_cast<unsigned char>(body[0]))) {
      while (j < body.size() && std::isdigit(static_cast<unsigned char>(body[j]))) ++j;
    } else {
      while (j < body.size() && (std::isalnum(static_cast<unsigned char>(body[j])) || body[j] == '_')) ++j;
    }
    std::string name = body.substr(0, j), rest = body.substr(j);
    if (name.empty()) throw ExpandError("${" + body + "}: bad substitution");
    if ((name == "@" || name == "*") && rest.empty()) {
      positional(name[0], dq);
      return;
    }
    std::optional<std::string> v = special(name);
    if (rest.empty()) {
      add(v.value_or(""), fl);
      return;
    }
    static const char* kOps[] = {":-", ":=", ":+", ":?", "##", "%%", "-", "=", "+", "?", "#", "%"};
    std::string op;
    for (const char* o : kOps) {
      if (rest.compare(0, std::strlen(o), o) == 0) {
        op = o;
        break;
      }
    }
    if (op.empty()) throw ExpandError("${" + body + "}: bad substitution");
    std::string word = rest.substr(op.size());
    bool colon = op[0] == ':' && op.size() == 2;
    bool unset_or_null = !v || (colon && v->empty());
    char o = colon ? op[1] : op[0];
    if (op == "##" || op == "%%" || op == "#" || op == "%") {
      add(trim(v.value_or(""), sh_.expand_string(word), op), fl);
    } else if (o == '-') {
      add(unset_or_null ? sh_.expand_string(word) : *v, fl);
    } else if (o == '=') {
      if (unset_or_null) {
        if (!is_name(name)) throw ExpandError("$" + name + ": cannot assign in this way");
        sh_.set(name, sh_.expand_string(word));
        v = sh_.get(name);
      }
      add(*v, fl);
    } else if (o == '+') {
      if (!unset_or_null) add(sh_.expand_string(word), fl);
    } else if (o == '?') {
      if (unset_or_null) {
        std::string msg = sh_.expand_string(word);
        throw ExpandError(name + ": " + (msg.empty() ? "parameter null or not set" : msg));
      }
      add(*v, fl);
    }
  }

  static std::string trim(const std::string& v, const std::string& pat, const std::string& op) {
    bool longest = op.size() == 2;
    size_t n = v.size();
    if (op[0] == '#') {
      for (size_t k = 0; k <= n; ++k) {
        size_t len = longest ? n - k : k;
        if (fnmatch(pat.c_str(), v.substr(0, len).c_str(), 0) == 0) return v.substr(len);
      }
    } else {
      for (size_t k = 0; k <= n; ++k) {
        size_t start = longest ? k : n - k;
        if (fnmatch(pat.c_str(), v.substr(start).c_str(), 0) == 0) return v.substr(0, start);
      }
    }
    return v;
  }

  size_t backtick(const std::string& raw, size_t i, bool dq) {
    size_t end = match_backtick(raw, i + 1);
    std::string inner;
    for (size_t k = i + 1; k < end; ++k) {
      if (raw[k] == '\\' && k + 1 < end && std::strchr("`\\$", raw[k + 1])) ++k;
      inner += raw[k];
    }
    add(sh_.command_output(inner), dq ? kQuoted : kExpanded);
    return end + 1;
  }

  // POSIX field splitting of the unquoted expansion results in p.
  void split_piece(const Piece& p, std::vector<Piece>& out) {
    std::string ifs = sh_.get("IFS").value_or(" \t\n");
    size_t before = out.size();
    Piece f;
    bool started = false;
    for (size_t k = 0; k < p.s.size(); ++k) {
      char c = p.s[k];
      if ((p.f[k] & kSplitBit) && ifs.find(c) != std::string::npos) {
        bool ws = c == ' ' || c == '\t' || c == '\n';
        if (started || !ws) out.push_back(f);
        f = Piece();
        started = false;
        continue;
      }
      f.s += c;
      f.f += p.f[k];
      started = true;
    }
    if (started || (p.exists && out.size() == before)) out.push_back(f);
  }

  void glob_piece(const Piece& p, std::vector<std::string>& out) {
    if (!split_) {
      out.push_back(p.s);
      return;
    }
    bool magic = false;
    std::string pat;
    for (size_t k = 0; k < p.s.size(); ++k) {
      char c = p.s[k];
      bool g = p.f[k] & kGlobBit;
      if (std::strchr("*?[", c) && g) magic = true;
      if (std::strchr("*?[]\\", c) && !g) pat += '\\';
      pat += c;
    }
    if (magic) {
      glob_t gl{};
      if (glob(pat.c_str(), 0, nullptr, &gl) == 0) {
        for (size_t k = 0; k < gl.gl_pathc; ++k) out.push_back(gl.gl_pathv[k]);
        globfree(&gl);
        return;
      }
      globfree(&gl);
    }
    out.push_back(p.s);
  }

  Shell& sh_;
  bool split_;
  std::vector<Piece> pieces_;
};

}  // namespace

std::vector<std::string> Shell::expand_word(const std::string& raw) {
  Expander e(*this, true);
  e.word(raw);
  return e.fields();
}

std::vector<std::string> Shell::expand_words(const std::vector<std::string>& raw) {
  std::vector<std::string> out;
  for (const std::string& w : raw) {
    auto f = expand_word(w);
    out.insert(out.end(), f.begin(), f.end());
  }
  return out;
}

std::string Shell::expand_string(const std::string& raw) {
  Expander e(*this, false);
  e.word(raw);
  return e.joined();
}

std::string Shell::expand_prompt(const std::string& ps) {
  std::string out;
  for (size_t i = 0; i < ps.size(); ++i) {
    char c = ps[i];
    if (c != '\\' || i + 1 >= ps.size()) {
      out += c;
      continue;
    }
    char e = ps[++i];
    switch (e) {
      case 'u': {
        const passwd* pw = getpwuid(geteuid());
        out += pw ? pw->pw_name : std::to_string(geteuid());
        break;
      }
      case 'h':
      case 'H': {
        char host[256] = {0};
        gethostname(host, sizeof host - 1);
        std::string h = host;
        if (e == 'h' && h.find('.') != std::string::npos) h.resize(h.find('.'));
        out += h;
        break;
      }
      case 'w':
      case 'W': {
        std::string pwd = get_or("PWD");
        if (pwd.empty()) {
          char buf[4096];
          if (getcwd(buf, sizeof buf)) pwd = buf;
        }
        std::string home = get_or("HOME");
        if (e == 'W') {
          if (pwd == home && !home.empty()) {
            out += "~";
          } else if (pwd == "/") {
            out += "/";
          } else {
            out += pwd.substr(pwd.rfind('/') + 1);
          }
        } else if (!home.empty() && home != "/" &&
                   (pwd == home || (pwd.compare(0, home.size(), home) == 0 && pwd[home.size()] == '/'))) {
          out += "~" + pwd.substr(home.size());
        } else {
          out += pwd;
        }
        break;
      }
      case '$':
        out += geteuid() == 0 ? '#' : '$';
        break;
      case 'n':
        out += '\n';
        break;
      case 'e':
        out += '\033';
        break;
      case 't': {
        char buf[16];
        time_t now = time(nullptr);
        strftime(buf, sizeof buf, "%H:%M:%S", localtime(&now));
        out += buf;
        break;
      }
      case 'j':
        out += std::to_string(jobs.size());
        break;
      case '?':
        out += std::to_string(last_status);
        break;
      case '\\':
        out += '\\';
        break;
      case '[':
      case ']':
        break;
      default:
        out += '\\';
        out += e;
    }
  }
  return out;
}

}  // namespace msh
