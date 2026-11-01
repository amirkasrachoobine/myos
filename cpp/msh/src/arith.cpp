// $(( ... )): 64-bit integer arithmetic with C operators and precedence,
// including assignments (=, +=, ...) and ?:. Variables that aren't set or
// are empty count as 0.

#include <cctype>
#include <cstring>

#include "shell.hpp"

namespace msh {

namespace {

class Arith {
 public:
  Arith(Shell& sh, const std::string& s, int depth) : sh_(sh), s_(s), depth_(depth) {}

  long long run() {
    skip();
    if (p_ >= s_.size()) return 0;
    long long v = assign();
    skip();
    if (p_ < s_.size()) fail("syntax error: invalid arithmetic operator (error token is \"" + s_.substr(p_) + "\")");
    return v;
  }

 private:
  struct Value {
    long long v;
    std::string name;  // set when this is a bare variable (assignable)
  };

  [[noreturn]] void fail(const std::string& m) { throw ExpandError(s_ + ": " + m); }
  void skip() {
    while (p_ < s_.size() && std::isspace(static_cast<unsigned char>(s_[p_]))) ++p_;
  }
  bool eat(const char* op) {
    skip();
    size_t n = std::strlen(op);
    if (s_.compare(p_, n, op) != 0) return false;
    // Don't take `<` out of `<<`, `&` out of `&&`, `=` out of `==`, ...
    if (n == 1 && p_ + 1 < s_.size()) {
      char a = op[0], b = s_[p_ + 1];
      if ((a == '<' || a == '>' || a == '&' || a == '|' || a == '=') && b == a) return false;
      if (std::strchr("<>!=", a) && b == '=') return false;
      if (std::strchr("+-*/%&|^", a) && b == '=') return false;
    }
    if (n == 2 && p_ + 2 < s_.size() && (std::strcmp(op, "<<") == 0 || std::strcmp(op, ">>") == 0) &&
        s_[p_ + 2] == '=') {
      return false;
    }
    p_ += n;
    return true;
  }

  long long var(const std::string& name) {
    std::string v = sh_.get_or(name);
    if (v.empty()) return 0;
    if (depth_ > 32) fail("expression recursion level exceeded");
    return Arith(sh_, v, depth_ + 1).run();
  }

  long long assign() {
    size_t save = p_;
    skip();
    if (p_ < s_.size() && (std::isalpha(static_cast<unsigned char>(s_[p_])) || s_[p_] == '_')) {
      size_t b = p_;
      while (p_ < s_.size() && (std::isalnum(static_cast<unsigned char>(s_[p_])) || s_[p_] == '_')) ++p_;
      std::string name = s_.substr(b, p_ - b);
      static const char* kOps[] = {"<<=", ">>=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "="};
      skip();
      for (const char* op : kOps) {
        size_t n = std::strlen(op);
        if (s_.compare(p_, n, op) == 0 && !(n == 1 && p_ + 1 < s_.size() && s_[p_ + 1] == '=')) {
          p_ += n;
          long long rhs = assign();
          long long cur = n > 1 ? var(name) : 0;
          long long v = rhs;
          std::string o(op, n - 1);
          if (o == "+") v = cur + rhs;
          if (o == "-") v = cur - rhs;
          if (o == "*") v = cur * rhs;
          if (o == "/" || o == "%") {
            if (rhs == 0) fail("division by 0");
            v = o == "/" ? cur / rhs : cur % rhs;
          }
          if (o == "&") v = cur & rhs;
          if (o == "|") v = cur | rhs;
          if (o == "^") v = cur ^ rhs;
          if (o == "<<") v = cur << rhs;
          if (o == ">>") v = cur >> rhs;
          sh_.set(name, std::to_string(v));
          return v;
        }
      }
    }
    p_ = save;
    return ternary();
  }

  long long ternary() {
    long long c = binary(0);
    if (eat("?")) {
      long long a = assign();
      if (!eat(":")) fail("expected `:'");
      long long b = assign();
      return c ? a : b;
    }
    return c;
  }

  // Binary operators by precedence level, lowest first.
  long long binary(int level) {
    static const std::vector<std::vector<const char*>> kLevels = {
        {"||"}, {"&&"}, {"|"}, {"^"}, {"&"}, {"==", "!="}, {"<=", ">=", "<", ">"}, {"<<", ">>"}, {"+", "-"},
        {"*", "/", "%"}};
    if (level == static_cast<int>(kLevels.size())) return unary();
    long long v = binary(level + 1);
    for (;;) {
      const char* hit = nullptr;
      for (const char* op : kLevels[level]) {
        if (eat(op)) {
          hit = op;
          break;
        }
      }
      if (!hit) return v;
      std::string o = hit;
      if (o == "||") {
        long long r = binary(level + 1);
        v = v || r;
        continue;
      }
      if (o == "&&") {
        long long r = binary(level + 1);
        v = v && r;
        continue;
      }
      long long r = binary(level + 1);
      if (o == "|") v |= r;
      else if (o == "^") v ^= r;
      else if (o == "&") v &= r;
      else if (o == "==") v = v == r;
      else if (o == "!=") v = v != r;
      else if (o == "<=") v = v <= r;
      else if (o == ">=") v = v >= r;
      else if (o == "<") v = v < r;
      else if (o == ">") v = v > r;
      else if (o == "<<") v <<= r;
      else if (o == ">>") v >>= r;
      else if (o == "+") v += r;
      else if (o == "-") v -= r;
      else if (o == "*") v *= r;
      else {
        if (r == 0) fail("division by 0");
        v = o == "/" ? v / r : v % r;
      }
    }
  }

  long long unary() {
    if (eat("+")) return unary();
    if (eat("-")) return -unary();
    if (eat("!")) return !unary();
    if (eat("~")) return ~unary();
    return primary();
  }

  long long primary() {
    skip();
    if (p_ >= s_.size()) fail("syntax error: operand expected");
    char c = s_[p_];
    if (c == '(') {
      ++p_;
      long long v = assign();
      if (!eat(")")) fail("expected `)'");
      return v;
    }
    if (std::isdigit(static_cast<unsigned char>(c))) {
      size_t b = p_;
      while (p_ < s_.size() && std::isalnum(static_cast<unsigned char>(s_[p_]))) ++p_;
      std::string num = s_.substr(b, p_ - b);
      try {
        size_t used = 0;
        long long v = std::stoll(num, &used, 0);
        if (used != num.size()) fail("value too great for base (error token is \"" + num + "\")");
        return v;
      } catch (const std::logic_error&) {
        fail("invalid number \"" + num + "\"");
      }
    }
    if (std::isalpha(static_cast<unsigned char>(c)) || c == '_') {
      size_t b = p_;
      while (p_ < s_.size() && (std::isalnum(static_cast<unsigned char>(s_[p_])) || s_[p_] == '_')) ++p_;
      return var(s_.substr(b, p_ - b));
    }
    fail("syntax error: operand expected (error token is \"" + s_.substr(p_) + "\")");
  }

  Shell& sh_;
  const std::string& s_;
  size_t p_ = 0;
  int depth_;
};

}  // namespace

long long Shell::arith(const std::string& expr) { return Arith(*this, expr, 0).run(); }

}  // namespace msh
