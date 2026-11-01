// msh's line editor: Emacs-style keys, history, Ctrl-R search and Tab
// completion, on any VT100-compatible terminal (including a serial
// console that can't report its size: it is asked with a cursor position
// report instead).

#pragma once

#include <termios.h>

#include <functional>
#include <string>
#include <vector>

namespace msh {

class History {
 public:
  // Reads `path` (one entry per line), keeping the newest `max` entries.
  void load(const std::string& path, size_t max);
  // Adds an entry and appends it to the file. Skips blank lines, lines
  // starting with a space, and repeats of the previous entry.
  void add(const std::string& line);
  void clear();
  const std::vector<std::string>& entries() const { return e_; }

 private:
  std::vector<std::string> e_;
  std::string path_;
  size_t max_ = 1000;
};

struct Completion {
  size_t start = 0;                  // the text being completed is line[start, cursor)
  std::vector<std::string> replace;  // what each match puts there (already quoted)
  std::vector<std::string> display;  // how each match is listed
  std::vector<char> suffix;          // added after a unique match: ' ', '/' or 0
};
using Completer = std::function<Completion(const std::string& line, size_t cursor)>;

class LineEditor {
 public:
  LineEditor(int in_fd, int out_fd, History& history);

  enum class Status { Ok, Eof, Interrupted };
  // Reads one line. The prompt may contain newlines and colour escapes.
  Status read_line(const std::string& prompt, std::string& line);

  // Works out the terminal width. If the kernel doesn't know it (a serial
  // console), asks the terminal and tells the kernel, so programs started
  // from the shell see it too.
  void detect_size();
  int columns() const;

  Completer completer;

 private:
  bool raw_on();
  void raw_off();
  int read_key();
  bool read_byte(unsigned char& c, int timeout_ms);
  bool input_pending() const;

  void insert(const std::string& s);
  void refresh();
  void move_to_end_row();
  void reprint_prompt();
  void complete();
  int search();  // Ctrl-R; returns a key still to be handled, 0 for none, -1 for EOF
  void history_move(int delta);
  void list_matches(const Completion& c);

  int in_, out_;
  History& hist_;
  termios orig_{};
  bool raw_ = false;
  int cached_cols_ = 0;

  std::string buf_;
  size_t pos_ = 0;
  std::string prompt_full_, prompt_last_;
  bool dirty_ = false;
  int cursor_row_ = 0;  // cursor's row, counted from the prompt's last line
  int max_rows_ = 1;    // rows the line has used since it was last drawn from scratch
  size_t hist_idx_ = 0;
  std::string hist_saved_;
  std::string kill_;
  bool last_tab_ = false;
};

// Width on screen, skipping escape sequences and UTF-8 continuation bytes.
int display_width(const std::string& s);

}  // namespace msh
