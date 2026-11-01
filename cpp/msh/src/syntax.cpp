#include "syntax.hpp"

#include <cctype>
#include <cstring>
#include <set>

namespace msh {

namespace {

bool is_meta(char c) { return std::strchr(" \t\n;&|<>()", c) != nullptr; }

size_t skip_squote(const std::string& s, size_t i) {  // s[i] is the opening '
  size_t j = s.find('\'', i + 1);
  if (j == std::string::npos) throw Incomplete();
  return j;
}

size_t skip_dquote(const std::string& s, size_t i) {  // s[i] is the opening "
  for (size_t j = i + 1; j < s.size(); ++j) {
    char c = s[j];
    if (c == '\\') {
      ++j;
    } else if (c == '"') {
      return j;
    } else if (c == '`') {
      j = match_backtick(s, j + 1);
    } else if (c == '$' && j + 1 < s.size() && s[j + 1] == '(') {
      j = (j + 2 < s.size() && s[j + 2] == '(') ? match_arith(s, j + 3) + 1 : match_paren(s, j + 2);
    } else if (c == '$' && j + 1 < s.size() && s[j + 1] == '{') {
      j = match_brace(s, j + 2);
    }
  }
  throw Incomplete();
}

}  // namespace

size_t match_paren(const std::string& s, size_t i) {
  int depth = 1;
  for (size_t j = i; j < s.size(); ++j) {
    char c = s[j];
    if (c == '\\') {
      ++j;
    } else if (c == '\'') {
      j = skip_squote(s, j);
    } else if (c == '"') {
      j = skip_dquote(s, j);
    } else if (c == '`') {
      j = match_backtick(s, j + 1);
    } else if (c == '(') {
      ++depth;
    } else if (c == ')') {
      if (--depth == 0) return j;
    }
  }
  throw Incomplete();
}

size_t match_brace(const std::string& s, size_t i) {
  int depth = 1;
  for (size_t j = i; j < s.size(); ++j) {
    char c = s[j];
    if (c == '\\') {
      ++j;
    } else if (c == '\'') {
      j = skip_squote(s, j);
    } else if (c == '"') {
      j = skip_dquote(s, j);
    } else if (c == '$' && j + 1 < s.size() && s[j + 1] == '(') {
      j = match_paren(s, j + 2);
    } else if (c == '{') {
      ++depth;
    } else if (c == '}') {
      if (--depth == 0) return j;
    }
  }
  throw Incomplete();
}

size_t match_arith(const std::string& s, size_t i) {
  int depth = 0;
  for (size_t j = i; j < s.size(); ++j) {
    char c = s[j];
    if (c == '(') {
      ++depth;
    } else if (c == ')') {
      if (depth == 0) {
        if (j + 1 < s.size() && s[j + 1] == ')') return j;
        if (j + 1 >= s.size()) throw Incomplete();
        throw SyntaxError("missing `))' in arithmetic expansion");
      }
      --depth;
    } else if (c == '$' && j + 1 < s.size() && s[j + 1] == '(') {
      if (j + 2 < s.size() && s[j + 2] == '(') {
        j = match_arith(s, j + 3) + 1;
      } else {
        j = match_paren(s, j + 2);
      }
    }
  }
  throw Incomplete();
}

size_t match_backtick(const std::string& s, size_t i) {
  for (size_t j = i; j < s.size(); ++j) {
    if (s[j] == '\\') {
      ++j;
    } else if (s[j] == '`') {
      return j;
    }
  }
  throw Incomplete();
}

std::vector<Token> lex(const std::string& s) {
  static const char* kOps[] = {"&>>", "<<<", "&&", "||", ";;", ">>", "<<", ">&", "<&", ">|", "<>", "&>",
                               "|",   "&",   ";",  "<",  ">",  "(",  ")"};
  std::vector<Token> out;
  size_t i = 0, n = s.size();
  int pending_io = -1;
  while (i < n) {
    char c = s[i];
    if (c == ' ' || c == '\t') {
      ++i;
      continue;
    }
    if (c == '\\' && i + 1 < n && s[i + 1] == '\n') {
      i += 2;
      continue;
    }
    if (c == '#') {
      while (i < n && s[i] != '\n') ++i;
      continue;
    }
    if (c == '\n') {
      out.push_back({Tok::Newline, "\n", -1, i, i + 1});
      ++i;
      continue;
    }
    bool op = false;
    for (const char* o : kOps) {
      size_t len = std::strlen(o);
      if (s.compare(i, len, o) == 0) {
        out.push_back({Tok::Op, o, -1, i, i + len});
        if (o[0] == '<' || o[0] == '>') out.back().io_number = pending_io;
        pending_io = -1;
        i += len;
        op = true;
        break;
      }
    }
    if (op) continue;
    pending_io = -1;

    size_t start = i;
    std::string word;
    while (i < n && !is_meta(s[i])) {
      char ch = s[i];
      size_t j = i;
      if (ch == '\\') {
        if (i + 1 >= n) throw Incomplete();
        if (s[i + 1] == '\n') {  // line continuation inside a word
          i += 2;
          continue;
        }
        j = i + 1;
      } else if (ch == '\'') {
        j = skip_squote(s, i);
      } else if (ch == '"') {
        j = skip_dquote(s, i);
      } else if (ch == '`') {
        j = match_backtick(s, i + 1);
      } else if (ch == '$' && i + 1 < n && s[i + 1] == '(') {
        j = (i + 2 < n && s[i + 2] == '(') ? match_arith(s, i + 3) + 1 : match_paren(s, i + 2);
      } else if (ch == '$' && i + 1 < n && s[i + 1] == '{') {
        j = match_brace(s, i + 2);
      }
      word.append(s, i, j - i + 1);
      i = j + 1;
    }
    bool digits = !word.empty() && word.size() < 4;
    for (char d : word) digits = digits && std::isdigit(static_cast<unsigned char>(d));
    if (digits && i < n && (s[i] == '<' || s[i] == '>')) {
      pending_io = std::stoi(word);
      continue;
    }
    out.push_back({Tok::Word, word, -1, start, i});
  }
  out.push_back({Tok::End, "", -1, n, n});
  return out;
}

bool is_name(const std::string& s) {
  if (s.empty() || std::isdigit(static_cast<unsigned char>(s[0]))) return false;
  for (char c : s) {
    if (!std::isalnum(static_cast<unsigned char>(c)) && c != '_') return false;
  }
  return true;
}

bool is_assignment(const std::string& w) {
  size_t eq = w.find('=');
  return eq != std::string::npos && eq > 0 && is_name(w.substr(0, eq));
}

bool is_keyword(const std::string& w) {
  static const std::set<std::string> kw = {"if",   "then", "elif",  "else", "fi", "for", "in", "while",
                                           "until", "do",  "done", "{",    "}",  "!"};
  return kw.count(w) != 0;
}

namespace {

class Parser {
 public:
  Parser(const std::string& src, const AliasMap* aliases) : src_(src), aliases_(aliases), t_(lex(src)) {}

  NodeP program() {
    NodeP n = list({});
    if (peek().type != Tok::End) unexpected();
    return n;
  }

 private:
  const Token& peek() const { return t_[p_]; }
  bool at_op(const char* op) const { return peek().type == Tok::Op && peek().text == op; }
  bool at_word(const char* w) const { return peek().type == Tok::Word && peek().text == w; }
  bool at_redir() const {
    if (peek().type != Tok::Op) return false;
    char c = peek().text[0];
    return c == '<' || c == '>' || peek().text == "&>" || peek().text == "&>>";
  }
  void skip_newlines() {
    while (peek().type == Tok::Newline) ++p_;
  }

  [[noreturn]] void unexpected() const {
    if (peek().type == Tok::End) throw Incomplete();
    std::string what = peek().type == Tok::Newline ? "newline" : peek().text;
    throw SyntaxError("syntax error near unexpected token `" + what + "'");
  }

  void expect(const char* word) {
    if (peek().type == Tok::End) throw Incomplete();
    if (!at_word(word)) unexpected();
    ++p_;
  }

  bool at_stop(std::initializer_list<const char*> stops) const {
    if (peek().type != Tok::Word) return false;
    for (const char* s : stops) {
      if (peek().text == s) return true;
    }
    return false;
  }

  // A compound list: and-or lists separated by ; & or newlines, up to one
  // of `stops`, a `)`, or the end of input.
  NodeP list(std::initializer_list<const char*> stops) {
    auto n = std::make_shared<Node>(Node::List);
    for (;;) {
      skip_newlines();
      if (peek().type == Tok::End || at_op(")") || at_stop(stops)) break;
      n->kids.push_back(and_or());
      bool bg = false;
      if (at_op("&")) {
        bg = true;
        ++p_;
      } else if (at_op(";") || peek().type == Tok::Newline) {
        ++p_;
      } else if (!(peek().type == Tok::End || at_op(")") || at_stop(stops))) {
        unexpected();
      }
      n->background.push_back(bg);
    }
    return n;
  }

  // A body that must not be empty, e.g. between `then` and `fi`.
  NodeP body(std::initializer_list<const char*> stops) {
    NodeP n = list(stops);
    if (n->kids.empty()) unexpected();
    return n;
  }

  NodeP and_or() {
    size_t start = p_;
    auto n = std::make_shared<Node>(Node::AndOr);
    n->kids.push_back(pipeline());
    while (at_op("&&") || at_op("||")) {
      n->ops.push_back(peek().text);
      ++p_;
      skip_newlines();
      n->kids.push_back(pipeline());
    }
    n->text = text(start);
    return n;
  }

  NodeP pipeline() {
    size_t start = p_;
    auto n = std::make_shared<Node>(Node::Pipeline);
    if (at_word("!")) {
      n->negate = true;
      ++p_;
    }
    n->kids.push_back(command());
    while (at_op("|")) {
      ++p_;
      skip_newlines();
      n->kids.push_back(command());
    }
    n->text = text(start);
    return n;
  }

  NodeP command() {
    size_t start = p_;
    NodeP n;
    if (peek().type == Tok::End) throw Incomplete();
    if (at_op("(")) {
      ++p_;
      n = std::make_shared<Node>(Node::Subshell);
      n->body = list({});
      if (peek().type == Tok::End) throw Incomplete();
      if (!at_op(")") || n->body->kids.empty()) unexpected();
      ++p_;
    } else if (at_word("{")) {
      ++p_;
      n = std::make_shared<Node>(Node::Group);
      n->body = list({"}"});
      expect("}");
      if (n->body->kids.empty()) throw SyntaxError("syntax error near unexpected token `}'");
    } else if (at_word("if")) {
      ++p_;
      n = std::make_shared<Node>(Node::If);
      n->conds.push_back(body({"then"}));
      expect("then");
      n->bodies.push_back(body({"elif", "else", "fi"}));
      while (at_word("elif")) {
        ++p_;
        n->conds.push_back(body({"then"}));
        expect("then");
        n->bodies.push_back(body({"elif", "else", "fi"}));
      }
      if (at_word("else")) {
        ++p_;
        n->else_body = body({"fi"});
      }
      expect("fi");
    } else if (at_word("while") || at_word("until")) {
      n = std::make_shared<Node>(Node::While);
      n->until = peek().text == "until";
      ++p_;
      n->cond = body({"do"});
      expect("do");
      n->body = body({"done"});
      expect("done");
    } else if (at_word("for")) {
      ++p_;
      n = std::make_shared<Node>(Node::For);
      if (peek().type == Tok::End) throw Incomplete();
      if (peek().type != Tok::Word || !is_name(peek().text)) unexpected();
      n->name = peek().text;
      ++p_;
      skip_newlines();
      if (at_word("in")) {
        ++p_;
        n->has_in = true;
        while (peek().type == Tok::Word) n->items.push_back(t_[p_++].text);
        if (peek().type == Tok::End) throw Incomplete();
        if (!at_op(";") && peek().type != Tok::Newline) unexpected();
        ++p_;
      } else if (at_op(";")) {
        ++p_;
      }
      skip_newlines();
      expect("do");
      n->body = body({"done"});
      expect("done");
    } else if (peek().type == Tok::Word && is_keyword(peek().text) && peek().text != "!") {
      unexpected();
    } else if (peek().type == Tok::Word && p_ + 2 < t_.size() && t_[p_ + 1].type == Tok::Op &&
               t_[p_ + 1].text == "(" && t_[p_ + 2].type == Tok::Op && t_[p_ + 2].text == ")") {
      n = std::make_shared<Node>(Node::FuncDef);
      n->name = peek().text;
      if (!is_name(n->name) && n->name.find_first_of("'\"$\\`/") != std::string::npos) {
        throw SyntaxError("`" + n->name + "': not a valid function name");
      }
      p_ += 3;
      skip_newlines();
      if (!(at_word("{") || at_op("(") || at_word("if") || at_word("while") || at_word("until") ||
            at_word("for"))) {
        unexpected();
      }
      n->body = command();
      n->text = text(start);
      return n;
    } else {
      return simple();
    }
    while (at_redir()) n->redirs.push_back(redir());
    n->text = text(start);
    return n;
  }

  Redir redir() {
    const Token& op = t_[p_++];
    if (op.text == "<<" || op.text == "<<<") throw SyntaxError("here-documents aren't supported; use a file or a pipe");
    if (peek().type != Tok::Word) {
      if (peek().type == Tok::End) throw SyntaxError("syntax error near unexpected token `newline'");
      unexpected();
    }
    return Redir{op.io_number, op.text, t_[p_++].text};
  }

  NodeP simple() {
    size_t start = p_;
    auto n = std::make_shared<Node>(Node::Simple);
    std::set<std::string> expanded;
    for (;;) {
      if (peek().type == Tok::Word) {
        const std::string& w = peek().text;
        if (n->words.empty() && is_assignment(w)) {
          n->assigns.push_back(w);
          ++p_;
        } else if (n->words.empty() && aliases_ && aliases_->count(w) && !expanded.count(w) &&
                   w.find_first_of("'\"\\") == std::string::npos) {
          expanded.insert(w);
          std::vector<Token> repl = lex(aliases_->at(w));
          repl.pop_back();  // End
          for (Token& r : repl) {
            r.begin = peek().begin;
            r.end = peek().end;
          }
          t_.erase(t_.begin() + p_);
          t_.insert(t_.begin() + p_, repl.begin(), repl.end());
          // An alias may expand to nothing, or start with an operator.
          if (repl.empty() || repl[0].type != Tok::Word) break;
        } else {
          n->words.push_back(w);
          ++p_;
        }
      } else if (at_redir()) {
        n->redirs.push_back(redir());
      } else {
        break;
      }
    }
    if (n->words.empty() && n->assigns.empty() && n->redirs.empty()) unexpected();
    n->text = text(start);
    return n;
  }

  std::string text(size_t start_tok) const {
    if (p_ == 0 || start_tok >= p_) return "";
    size_t b = t_[start_tok].begin, e = t_[p_ - 1].end;
    return e > b ? src_.substr(b, e - b) : "";
  }

  const std::string& src_;
  const AliasMap* aliases_;
  std::vector<Token> t_;
  size_t p_ = 0;
};

}  // namespace

NodeP parse(const std::string& src, const AliasMap* aliases) { return Parser(src, aliases).program(); }

}  // namespace msh
