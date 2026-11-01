#include "lineedit.hpp"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include <algorithm>
#include <cctype>
#include <fstream>

#include "shell.hpp"

namespace msh {

namespace {

enum Key {
  kUp = 1000,
  kDown,
  kLeft,
  kRight,
  kHome,
  kEnd,
  kDelete,
  kWordLeft,
  kWordRight,
  kKillWordRight,
  kKillWordLeft,
  kIgnore,
};

bool is_cont(char c) { return (static_cast<unsigned char>(c) & 0xC0) == 0x80; }
bool is_word(char c) { return std::isalnum(static_cast<unsigned char>(c)) || c == '_' || (c & 0x80); }

std::string csi(int n, char f) { return "\x1b[" + std::to_string(n) + f; }

}  // namespace

int display_width(const std::string& s) {
  int w = 0;
  for (size_t i = 0; i < s.size(); ++i) {
    char c = s[i];
    if (c == '\x1b') {
      if (i + 1 < s.size() && s[i + 1] == '[') {
        i += 2;
        while (i < s.size() && !(s[i] >= 0x40 && s[i] <= 0x7e)) ++i;
      } else if (i + 1 < s.size() && s[i + 1] == ']') {  // OSC, e.g. a terminal title
        while (i < s.size() && s[i] != '\a') ++i;
      } else {
        ++i;
      }
      continue;
    }
    if (c == '\001' || c == '\002' || is_cont(c)) continue;
    ++w;
  }
  return w;
}

// --- history -------------------------------------------------------------------------

void History::load(const std::string& path, size_t max) {
  path_ = path;
  max_ = max;
  std::ifstream f(path);
  std::string line;
  size_t total = 0;
  while (std::getline(f, line)) {
    ++total;
    if (!line.empty()) e_.push_back(line);
  }
  if (e_.size() > max_) e_.erase(e_.begin(), e_.end() - max_);
  if (total > max_ + max_ / 2 && !path_.empty()) {  // keep the file from growing forever
    std::string tmp = path_ + ".tmp";
    std::ofstream o(tmp, std::ios::trunc);
    for (auto& e : e_) o << e << "\n";
    o.close();
    if (o) rename(tmp.c_str(), path_.c_str());
  }
}

void History::add(const std::string& line) {
  if (line.empty() || line[0] == ' ' || line.find_first_not_of(" \t") == std::string::npos) return;
  if (!e_.empty() && e_.back() == line) return;
  e_.push_back(line);
  if (e_.size() > max_) e_.erase(e_.begin());
  if (path_.empty()) return;
  int fd = open(path_.c_str(), O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
  if (fd < 0) return;
  write_all(fd, line + "\n");
  close(fd);
}

void History::clear() {
  e_.clear();
  if (!path_.empty()) {
    int fd = open(path_.c_str(), O_WRONLY | O_TRUNC | O_CLOEXEC);
    if (fd >= 0) close(fd);
  }
}

// --- terminal --------------------------------------------------------------------------

LineEditor::LineEditor(int in_fd, int out_fd, History& history) : in_(in_fd), out_(out_fd), hist_(history) {}

bool LineEditor::raw_on() {
  if (raw_) return true;
  if (tcgetattr(in_, &orig_) != 0) return false;
  termios raw = orig_;
  raw.c_iflag &= ~(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
  raw.c_cflag |= CS8;
  raw.c_lflag &= ~(ECHO | ICANON | IEXTEN | ISIG);
  raw.c_cc[VMIN] = 1;
  raw.c_cc[VTIME] = 0;
  // TCSANOW, not TCSAFLUSH: keep whatever was typed ahead.
  if (tcsetattr(in_, TCSANOW, &raw) != 0) return false;
  raw_ = true;
  return true;
}

void LineEditor::raw_off() {
  if (!raw_) return;
  tcsetattr(in_, TCSANOW, &orig_);
  raw_ = false;
}

int LineEditor::columns() const {
  winsize ws{};
  if (ioctl(in_, TIOCGWINSZ, &ws) == 0 && ws.ws_col > 0) return ws.ws_col;
  return cached_cols_ > 0 ? cached_cols_ : 80;
}

void LineEditor::detect_size() {
  winsize ws{};
  if (ioctl(in_, TIOCGWINSZ, &ws) == 0 && ws.ws_col > 0) return;
  bool was_raw = raw_;
  if (!raw_on()) return;
  // Save the cursor, go far down-right, ask where we ended up, restore.
  write_all(out_, "\x1b" "7\x1b[999;999H\x1b[6n\x1b" "8");
  std::string reply;
  unsigned char c;
  while (reply.size() < 32 && read_byte(c, 300)) {
    reply += static_cast<char>(c);
    if (c == 'R') break;
  }
  if (!was_raw) raw_off();
  size_t b = reply.find("\x1b[");
  int rows = 0, cols = 0;
  if (b != std::string::npos && std::sscanf(reply.c_str() + b, "\x1b[%d;%dR", &rows, &cols) == 2 && cols > 0) {
    cached_cols_ = cols;
    ws.ws_row = static_cast<unsigned short>(rows);
    ws.ws_col = static_cast<unsigned short>(cols);
    ioctl(in_, TIOCSWINSZ, &ws);  // programs we start (top, ls) see it now
  }
}

bool LineEditor::input_pending() const {
  pollfd p{in_, POLLIN, 0};
  return poll(&p, 1, 0) > 0;
}

bool LineEditor::read_byte(unsigned char& c, int timeout_ms) {
  for (;;) {
    if (timeout_ms >= 0) {
      pollfd p{in_, POLLIN, 0};
      int r = poll(&p, 1, timeout_ms);
      if (r < 0 && errno == EINTR) {
        if (g_sighup) return false;
        continue;
      }
      if (r <= 0) return false;
    }
    ssize_t n = read(in_, &c, 1);
    if (n == 1) return true;
    if (n < 0 && errno == EINTR) {
      if (g_sighup) return false;
      if (g_sigwinch) {
        g_sigwinch = 0;
        refresh();
      }
      continue;
    }
    return false;
  }
}

int LineEditor::read_key() {
  unsigned char c;
  if (!read_byte(c, -1)) return -1;
  if (c != 27) return c;
  unsigned char d;
  if (!read_byte(d, 50)) return 27;
  if (d == '[') {
    std::string params;
    unsigned char f;
    for (;;) {
      if (!read_byte(f, 50)) return kIgnore;
      if (f >= 0x40 && f <= 0x7e) break;
      params += static_cast<char>(f);
    }
    bool mod = params.find(';') != std::string::npos;
    switch (f) {
      case 'A': return kUp;
      case 'B': return kDown;
      case 'C': return mod ? kWordRight : kRight;
      case 'D': return mod ? kWordLeft : kLeft;
      case 'H': return kHome;
      case 'F': return kEnd;
      case '~':
        if (params == "1" || params == "7") return kHome;
        if (params == "4" || params == "8") return kEnd;
        if (params == "3") return kDelete;
        return kIgnore;
      default:
        return kIgnore;  // including a late cursor position report
    }
  }
  if (d == 'O') {
    unsigned char f;
    if (!read_byte(f, 50)) return kIgnore;
    switch (f) {
      case 'A': return kUp;
      case 'B': return kDown;
      case 'C': return kRight;
      case 'D': return kLeft;
      case 'H': return kHome;
      case 'F': return kEnd;
      default: return kIgnore;
    }
  }
  if (d == 'b') return kWordLeft;
  if (d == 'f') return kWordRight;
  if (d == 'd') return kKillWordRight;
  if (d == 127 || d == 8) return kKillWordLeft;
  return kIgnore;
}

// --- drawing -------------------------------------------------------------------------------

void LineEditor::insert(const std::string& s) {
  if (s.empty()) return;
  if (pos_ == buf_.size() && !dirty_) {
    buf_ += s;
    pos_ = buf_.size();
    write_all(out_, s);
    int cols = columns();
    int total = display_width(prompt_last_) + display_width(buf_);
    cursor_row_ = total > 0 ? (total - 1) / cols : 0;
    max_rows_ = std::max(max_rows_, cursor_row_ + 1);
    return;
  }
  buf_.insert(pos_, s);
  pos_ += s.size();
  dirty_ = true;
}

void LineEditor::refresh() {
  int cols = columns();
  int plen = display_width(prompt_last_);
  int total = plen + display_width(buf_);
  int rows = std::max(1, (total + cols - 1) / cols);
  std::string o;
  int down = max_rows_ - 1 - cursor_row_;
  if (down > 0) o += csi(down, 'B');
  for (int j = 0; j < max_rows_ - 1; ++j) o += "\r\x1b[0K\x1b[1A";
  o += "\r\x1b[0K" + prompt_last_ + buf_;
  bool exact = total > 0 && total % cols == 0;
  int end_row;
  if (exact && pos_ == buf_.size()) {
    o += "\n";  // leave the pending wrap, so the cursor shows on the next row
    end_row = total / cols;
  } else {
    end_row = exact ? total / cols - 1 : total / cols;
  }
  int cpos = plen + display_width(buf_.substr(0, pos_));
  int target = cpos / cols;
  if (exact && pos_ == buf_.size()) target = end_row;
  if (end_row - target > 0) o += csi(end_row - target, 'A');
  o += "\r";
  if (cpos % cols) o += csi(cpos % cols, 'C');
  write_all(out_, o);
  cursor_row_ = target;
  max_rows_ = std::max(rows, end_row + 1);
  dirty_ = false;
}

void LineEditor::move_to_end_row() {
  int down = max_rows_ - 1 - cursor_row_;
  if (down > 0) write_all(out_, csi(down, 'B'));
  cursor_row_ = max_rows_ - 1;
}

void LineEditor::reprint_prompt() {
  size_t nl = prompt_full_.rfind('\n');
  if (nl != std::string::npos) write_all(out_, prompt_full_.substr(0, nl + 1));
  cursor_row_ = 0;
  max_rows_ = 1;
  refresh();
}

// --- history and search ------------------------------------------------------------------------

void LineEditor::history_move(int delta) {
  const auto& e = hist_.entries();
  long next = static_cast<long>(hist_idx_) + delta;
  if (next < 0 || next > static_cast<long>(e.size())) {
    write_all(out_, "\a");
    return;
  }
  if (hist_idx_ == e.size()) hist_saved_ = buf_;
  hist_idx_ = static_cast<size_t>(next);
  buf_ = hist_idx_ == e.size() ? hist_saved_ : e[hist_idx_];
  pos_ = buf_.size();
  dirty_ = true;
}

int LineEditor::search() {
  const auto& e = hist_.entries();
  std::string saved_prompt = prompt_last_, saved_buf = buf_;
  size_t saved_pos = pos_;
  std::string q;
  long idx = static_cast<long>(e.size());
  bool failed = false;
  auto find = [&](long from) -> long {
    for (long k = std::min(from, static_cast<long>(e.size()) - 1); k >= 0; --k) {
      if (e[k].find(q) != std::string::npos) return k;
    }
    return -1;
  };
  auto render = [&]() {
    prompt_last_ = std::string(failed ? "(failed reverse-i-search)`" : "(reverse-i-search)`") + q + "': ";
    if (idx < static_cast<long>(e.size())) {
      buf_ = e[idx];
      size_t at = buf_.find(q);
      pos_ = at == std::string::npos ? buf_.size() : at;
    }
    refresh();
  };
  auto leave = [&]() {
    prompt_last_ = saved_prompt;
    dirty_ = true;
  };
  render();
  for (;;) {
    int k = read_key();
    if (k < 0) {
      leave();
      return -1;
    }
    if (k == 18) {  // Ctrl-R: the next older match
      long f = find(idx - 1);
      if (f >= 0) {
        idx = f;
      } else {
        write_all(out_, "\a");
      }
    } else if (k == 127 || k == 8) {
      if (!q.empty()) q.pop_back();
      long f = find(static_cast<long>(e.size()) - 1);
      failed = f < 0;
      if (f >= 0) idx = f;
    } else if (k == 7 || k == 3) {  // Ctrl-G, Ctrl-C: back to what was there
      buf_ = saved_buf;
      pos_ = saved_pos;
      leave();
      return 0;
    } else if (k == 27) {
      leave();
      return 0;
    } else if (k >= 32 && k < 256 && k != 127) {
      q += static_cast<char>(k);
      long f = find(idx < static_cast<long>(e.size()) ? idx : static_cast<long>(e.size()) - 1);
      failed = f < 0;
      if (f >= 0) idx = f;
    } else {
      leave();
      if (idx < static_cast<long>(e.size())) hist_idx_ = static_cast<size_t>(idx);
      return k;
    }
    render();
  }
}

// --- completion ------------------------------------------------------------------------------------

void LineEditor::list_matches(const Completion& c) {
  move_to_end_row();
  write_all(out_, "\r\n");
  size_t n = c.display.size();
  if (n > 100) {
    write_all(out_, "Display all " + std::to_string(n) + " possibilities? (y or n)");
    int k = read_key();
    write_all(out_, "\r\n");
    if (k != 'y' && k != 'Y') {
      reprint_prompt();
      return;
    }
  }
  int cols = columns();
  int width = 0;
  for (auto& d : c.display) width = std::max(width, display_width(d));
  width += 2;
  int ncol = std::max(1, cols / width);
  size_t nrow = (n + ncol - 1) / ncol;
  std::string o;
  for (size_t r = 0; r < nrow; ++r) {
    for (int k = 0; k < ncol; ++k) {
      size_t i = k * nrow + r;
      if (i >= n) break;
      o += c.display[i];
      if ((k + 1) * nrow + r < n) o += std::string(width - display_width(c.display[i]), ' ');
    }
    o += "\r\n";
  }
  write_all(out_, o);
  reprint_prompt();
}

void LineEditor::complete() {
  if (!completer) return;
  Completion c = completer(buf_, pos_);
  if (c.replace.empty()) {
    write_all(out_, "\a");
    return;
  }
  std::string cur = buf_.substr(c.start, pos_ - c.start);
  std::string text;
  if (c.replace.size() == 1) {
    text = c.replace[0];
    if (c.suffix[0] && !(pos_ < buf_.size() && buf_[pos_] == c.suffix[0])) text += c.suffix[0];
  } else {
    text = c.replace[0];
    for (auto& r : c.replace) {
      size_t k = 0;
      while (k < text.size() && k < r.size() && text[k] == r[k]) ++k;
      text.resize(k);
    }
    while (!text.empty() && is_cont(text.back())) text.pop_back();
    if (text.size() <= cur.size()) {
      if (last_tab_) {
        list_matches(c);
      } else {
        write_all(out_, "\a");
      }
      return;
    }
  }
  if (text.compare(0, cur.size(), cur) == 0) {
    insert(text.substr(cur.size()));  // appending at the end draws without redrawing
  } else {
    buf_.replace(c.start, pos_ - c.start, text);
    pos_ = c.start + text.size();
    dirty_ = true;
  }
}

// --- the loop ----------------------------------------------------------------------------------------

LineEditor::Status LineEditor::read_line(const std::string& prompt, std::string& line) {
  line.clear();
  if (!raw_on()) {  // not a terminal after all: plain line reading
    write_all(out_, prompt);
    unsigned char c;
    bool any = false;
    while (read_byte(c, -1)) {
      any = true;
      if (c == '\n') return Status::Ok;
      line += static_cast<char>(c);
    }
    return any ? Status::Ok : Status::Eof;
  }
  buf_.clear();
  pos_ = 0;
  dirty_ = false;
  last_tab_ = false;
  hist_idx_ = hist_.entries().size();
  hist_saved_.clear();
  prompt_full_ = prompt;
  size_t nl = prompt.rfind('\n');
  prompt_last_ = nl == std::string::npos ? prompt : prompt.substr(nl + 1);
  write_all(out_, prompt);
  int cols = columns();
  int plen = display_width(prompt_last_);
  cursor_row_ = plen > 0 ? (plen - 1) / cols : 0;
  max_rows_ = cursor_row_ + 1;

  int pending = 0;
  for (;;) {
    int k = pending ? pending : read_key();
    pending = 0;
    bool was_tab = last_tab_;
    last_tab_ = false;
    if (k < 0) {
      raw_off();
      return Status::Eof;
    }
    switch (k) {
      case 13:
      case 10: {
        bool moved = dirty_ || pos_ != buf_.size();
        int cols2 = columns();
        int total = display_width(prompt_last_) + display_width(buf_);
        if (moved) {
          pos_ = buf_.size();
          refresh();
        }
        if (!(moved && total > 0 && total % cols2 == 0)) write_all(out_, "\n");
        raw_off();
        line = buf_;
        return Status::Ok;
      }
      case 3:
        move_to_end_row();
        write_all(out_, "^C\r\n");
        raw_off();
        return Status::Interrupted;
      case 4:
        if (buf_.empty()) {
          raw_off();
          return Status::Eof;
        }
        [[fallthrough]];
      case kDelete:
        if (pos_ < buf_.size()) {
          size_t e = pos_ + 1;
          while (e < buf_.size() && is_cont(buf_[e])) ++e;
          buf_.erase(pos_, e - pos_);
          dirty_ = true;
        }
        break;
      case 1:
      case kHome:
        pos_ = 0;
        dirty_ = true;
        break;
      case 5:
      case kEnd:
        if (pos_ != buf_.size()) {
          pos_ = buf_.size();
          dirty_ = true;
        }
        break;
      case 2:
      case kLeft:
        if (pos_ > 0) {
          do --pos_;
          while (pos_ > 0 && is_cont(buf_[pos_]));
          dirty_ = true;
        }
        break;
      case 6:
      case kRight:
        if (pos_ < buf_.size()) {
          do ++pos_;
          while (pos_ < buf_.size() && is_cont(buf_[pos_]));
          dirty_ = true;
        }
        break;
      case 127:
      case 8:
        if (pos_ > 0) {
          size_t b = pos_ - 1;
          while (b > 0 && is_cont(buf_[b])) --b;
          buf_.erase(b, pos_ - b);
          pos_ = b;
          dirty_ = true;
        }
        break;
      case 11:  // Ctrl-K
        kill_ = buf_.substr(pos_);
        buf_.erase(pos_);
        dirty_ = true;
        break;
      case 21:  // Ctrl-U
        kill_ = buf_.substr(0, pos_);
        buf_.erase(0, pos_);
        pos_ = 0;
        dirty_ = true;
        break;
      case 23: {  // Ctrl-W: the whitespace-delimited word before the cursor
        size_t b = pos_;
        while (b > 0 && std::isspace(static_cast<unsigned char>(buf_[b - 1]))) --b;
        while (b > 0 && !std::isspace(static_cast<unsigned char>(buf_[b - 1]))) --b;
        kill_ = buf_.substr(b, pos_ - b);
        buf_.erase(b, pos_ - b);
        pos_ = b;
        dirty_ = true;
        break;
      }
      case kKillWordLeft: {
        size_t b = pos_;
        while (b > 0 && !is_word(buf_[b - 1])) --b;
        while (b > 0 && is_word(buf_[b - 1])) --b;
        kill_ = buf_.substr(b, pos_ - b);
        buf_.erase(b, pos_ - b);
        pos_ = b;
        dirty_ = true;
        break;
      }
      case kKillWordRight: {
        size_t e = pos_;
        while (e < buf_.size() && !is_word(buf_[e])) ++e;
        while (e < buf_.size() && is_word(buf_[e])) ++e;
        kill_ = buf_.substr(pos_, e - pos_);
        buf_.erase(pos_, e - pos_);
        dirty_ = true;
        break;
      }
      case kWordLeft:
        while (pos_ > 0 && !is_word(buf_[pos_ - 1])) --pos_;
        while (pos_ > 0 && is_word(buf_[pos_ - 1])) --pos_;
        dirty_ = true;
        break;
      case kWordRight:
        while (pos_ < buf_.size() && !is_word(buf_[pos_])) ++pos_;
        while (pos_ < buf_.size() && is_word(buf_[pos_])) ++pos_;
        dirty_ = true;
        break;
      case 25:  // Ctrl-Y
        insert(kill_);
        break;
      case 20:  // Ctrl-T: swap the two characters before the cursor
        if (pos_ > 0 && buf_.size() > 1) {
          if (pos_ == buf_.size()) --pos_;
          if (!is_cont(buf_[pos_]) && !is_cont(buf_[pos_ - 1])) std::swap(buf_[pos_ - 1], buf_[pos_]);
          ++pos_;
          dirty_ = true;
        }
        break;
      case 12:  // Ctrl-L
        write_all(out_, "\x1b[H\x1b[2J");
        reprint_prompt();
        break;
      case 16:
      case kUp:
        history_move(-1);
        break;
      case 14:
      case kDown:
        history_move(1);
        break;
      case 18:
        pending = search();
        if (pending < 0) {
          raw_off();
          return Status::Eof;
        }
        break;
      case 9:
        last_tab_ = was_tab;
        complete();
        last_tab_ = true;
        break;
      default:
        if ((k >= 32 && k < 127) || (k >= 128 && k < 256)) insert(std::string(1, static_cast<char>(k)));
    }
    if (dirty_ && !pending && !input_pending()) refresh();
  }
}

}  // namespace msh
