// msh syntax: tokens, the command tree, and the parser.
//
// Words are kept as raw source text (quotes and $ included) and expanded
// only when the command runs, so `for f in *` globs on every iteration and
// `$(...)` runs when its command does.

#pragma once

#include <map>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace msh {

// The input ends inside a quote, $( ), an if/for/while, after a `|`, ...:
// an interactive shell asks for another line (PS2) and parses again.
struct Incomplete : std::runtime_error {
  Incomplete() : std::runtime_error("unexpected end of input") {}
};
struct SyntaxError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

enum class Tok { Word, Op, Newline, End };

struct Token {
  Tok type;
  std::string text;
  int io_number = -1;  // `2>`: the 2, on the redirection operator
  size_t begin = 0, end = 0;  // offsets in the source
};

// Splits source into tokens. Throws Incomplete for an unterminated quote
// or substitution, or a trailing backslash.
std::vector<Token> lex(const std::string& src);

// Given s[i] just past an opening "$(", "${", "$((" or "`", returns the
// index of the matching closer (")", "}", the first ")" of "))", or "`").
// Throws Incomplete if there is none.
size_t match_paren(const std::string& s, size_t i);
size_t match_brace(const std::string& s, size_t i);
size_t match_arith(const std::string& s, size_t i);
size_t match_backtick(const std::string& s, size_t i);

struct Redir {
  int fd;              // -1: the operator's default (0 for <, 1 for >)
  std::string op;      // < > >> >| <> <& >& &> &>>
  std::string target;  // raw word
};

struct Node;
using NodeP = std::shared_ptr<Node>;

struct Node {
  enum Kind { Simple, Pipeline, AndOr, List, If, While, For, Group, Subshell, FuncDef } kind;
  explicit Node(Kind k) : kind(k) {}

  // Simple
  std::vector<std::string> assigns;  // raw NAME=value
  std::vector<std::string> words;
  // Simple, and compound commands' trailing redirections
  std::vector<Redir> redirs;

  // Pipeline: commands; AndOr: pipelines; List: and-or lists
  std::vector<NodeP> kids;
  bool negate = false;               // Pipeline: `! cmd`
  std::vector<std::string> ops;      // AndOr: "&&" / "||" between kids
  std::vector<bool> background;      // List: kid followed by `&`

  // If: conds[i] then bodies[i]; else_body may be null
  std::vector<NodeP> conds, bodies;
  NodeP else_body;
  // While/For/Group/Subshell/FuncDef body; While condition
  NodeP cond, body;
  bool until = false;
  // For: variable, words after `in`; FuncDef: name
  std::string name;
  std::vector<std::string> items;
  bool has_in = false;

  std::string text;  // source text, for `jobs` and error messages
};

using AliasMap = std::map<std::string, std::string>;

// Parses a whole program. Aliases are expanded in command position.
NodeP parse(const std::string& src, const AliasMap* aliases = nullptr);

bool is_name(const std::string& s);       // valid variable name
bool is_assignment(const std::string& w);  // NAME=...
bool is_keyword(const std::string& w);

}  // namespace msh
