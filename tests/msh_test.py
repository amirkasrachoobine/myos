#!/usr/bin/env python3
"""Host-side tests for msh, the Phase 4 shell: no Buildroot build, no QEMU.

  scripts      each snippet runs under msh -c and under bash; the output and
               exit status must match.
  interactive  msh runs on a pseudo-terminal, the way it runs on the serial
               console: prompt, line editing, history (arrows, !!, Ctrl-R, the
               history file), Tab completion, multi-line commands, Ctrl-C, job
               control (Ctrl-Z, jobs, bg, fg, kill %1), and a console that
               can't report its size. With the `pyte` module (pip install
               pyte) it also checks what ends up on the screen.

Usage: python3 tests/msh_test.py [--msh PATH]   (default: builds cpp/msh into /tmp)
"""
import argparse
import fcntl
import os
import pathlib
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

try:
    import pyte
except ImportError:
    pyte = None

ROOT = pathlib.Path(__file__).resolve().parent.parent
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + ("" if ok or not detail else "\n      " + str(detail).strip().replace("\n", "\n      ")))


# --- scripts: msh -c vs bash ------------------------------------------------------

SCRIPTS = [
    "echo hello world",
    "echo -n a; echo b",
    "echo 'single $x' \"double $HOME_UNSET_X\" esc\\ aped",
    "x=1; y=$x$x; echo $y ${y}z",
    "echo ${unset_v:-default} ${unset_v-dash} ${e:=assigned} $e",
    "v=hello.tar.gz; echo ${v%.gz} ${v%%.*} ${v#*.} ${v##*.} ${#v}",
    "v=; echo [${v:+set}] [${v+set}] [${nope+set}]",
    "echo $((1 + 2 * 3)) $((10 / 3)) $((10 % 3)) $(( (1+2)*3 )) $((1<2)) $((5>3 && 2>3)) $((0x10)) $((-7/2))",
    "i=5; echo $((i += 2)) $i $((i * i)) $((i > 3 ? 100 : 200))",
    "echo $(echo inner) \"$(echo 'a  b')\" $(echo $(echo nested))",
    "echo `echo backtick`",
    "x='a  b   c'; for w in $x; do echo \"[$w]\"; done; echo \"[$x]\"",
    "set -- one 'two three' four; echo $#; for a in \"$@\"; do echo \"<$a>\"; done; echo \"$*\"",
    "set --; for a in \"$@\"; do echo never; done; echo none=$#",
    "IFS=:; x=a:b::c; for f in $x; do echo \"[$f]\"; done",
    "echo a | tr a b | tr b c",
    "printf '%s\\n' 3 1 2 | sort | head -2",
    "true && echo and; false && echo no; false || echo or; true || echo no",
    "! true; echo $?; ! false; echo $?",
    "false; echo $?; (exit 7); echo $?",
    "if true; then echo yes; else echo no; fi",
    "if false; then echo 1; elif false; then echo 2; else echo 3; fi",
    "for i in 1 2 3; do if [ $i = 2 ]; then continue; fi; echo $i; done",
    "for i in 1 2 3; do for j in a b; do [ $j = b ] && break 2; echo $i$j; done; done",
    "i=0; while [ $i -lt 5 ]; do i=$((i+1)); [ $i = 2 ] && continue; echo w$i; done",
    "i=0; until [ $i -ge 3 ]; do echo u$i; i=$((i+1)); done",
    "f() { echo \"f($#): $1 $2\"; return 4; }; f a 'b c'; echo $?",
    "f() { local v=inner; echo $v; }; v=outer; f; echo $v",
    "g() { echo $1; shift; echo $1; }; g x y",
    "{ echo one; echo two; } | wc -l",
    "(cd /; pwd); echo still-$(basename $PWD)",
    "x=out; (x=in; echo $x); echo $x",
    "echo data > $T/f1; cat < $T/f1; echo more >> $T/f1; cat $T/f1",
    "echo err >&2 2>/dev/null; echo ok",
    "ls /nonexistent-zz 2>/dev/null; echo st=$?",
    "{ echo out; echo err >&2; } > $T/both 2>&1; sort $T/both",
    "{ echo out; echo err >&2; } &> $T/amp; sort $T/amp",
    "exec 3> $T/fd3; echo via3 >&3; exec 3>&-; cat $T/fd3",
    "echo $(false); echo $?",
    "x=$(exit 3); echo $?",
    "echo a; # comment\necho b",
    "echo line1\necho line2",
    "for f in $T/g*; do echo $(basename $f); done",
    "echo $T/nomatch* | sed \"s|$T/||\"",
    "echo '*' \"*\" \\*",
    "cd $T; echo g?? | tr ' ' '\\n' | sort",
    "x=5; case_like() { [ $1 -gt 3 ] && echo big || echo small; }; case_like $x; case_like 1",
    "echo ~ | grep -c /",
    "A=1 B=2 sh -c 'echo $A$B'; echo [$A]",
    "export EX=exported; sh -c 'echo $EX'",
    "unset EX; echo [${EX-gone}]",
    "read a b <<EOF_NOT_SUPPORTED_SKIP\n",  # placeholder removed below
    "echo 'x y z' | { read a b; echo \"$a|$b\"; }",
    "printf 'l1\\nl2\\n' | while read l; do echo got:$l; done",
    "echo $((2**0)) 2>/dev/null; echo after",
    "eval 'echo evaled $((40+2))'",
    "type cd | head -1",
    "kill -l 9; kill -l TERM",
    "sleep 0.2 & wait $!; echo waited $?",
    "sh -c 'exit 3' & wait; echo $?",
    "echo \"a\\$b\\\\c\\\"d\"",
    "echo ${PWD:+has-pwd}",
    "x=abc; echo ${x:-$(echo fallback)} ${y:-$(echo fallback)}",
    "set -e; false || true; echo survived; false; echo not-reached",
    "for i in 1 2; do echo $i; done | cat",
    "while false; do :; done; echo st=$?",
    "echo $((7 > 3)) $((3 == 3)) $((3 != 3)) $((1 << 4)) $((255 & 15)) $((~0))",
]
SCRIPTS = [s for s in SCRIPTS if "EOF_NOT_SUPPORTED" not in s]
SCRIPTS = [s for s in SCRIPTS if "2**0" not in s]  # ** is bash-only


def run_scripts(msh, tmp):
    for name in ("ga1", "gb2", "gc3"):
        (tmp / name).write_text("")
    env = dict(os.environ, T=str(tmp), LC_ALL="C")
    env.pop("EX", None)
    fails = []
    for s in SCRIPTS:
        outs = []
        for shell in ([msh, "-c", s], ["bash", "--norc", "-c", s]):
            r = subprocess.run(shell, capture_output=True, text=True, env=env, cwd=tmp, timeout=20)
            outs.append((r.stdout, r.returncode))
        if outs[0] != outs[1]:
            fails.append(f"{s!r}\n  msh:  {outs[0]!r}\n  bash: {outs[1]!r}")
    check(f"{len(SCRIPTS)} scripts behave as in bash", not fails, "\n".join(fails))

    # Things bash does differently or msh reports its own way.
    def msh_out(script, stdin=None):
        r = subprocess.run([msh, "-c", script], capture_output=True, text=True, env=env, cwd=tmp, input=stdin, timeout=20)
        return r.stdout, r.stderr, r.returncode
    o, e, rc = msh_out("nosuchcmd-zz")
    check("unknown command: 127 and a message", rc == 127 and "nosuchcmd-zz: command not found" in e, (o, e, rc))
    o, e, rc = msh_out("echo 'unterminated")
    check("syntax error: status 2 and a message", rc == 2 and "unexpected end of file" in e, (o, e, rc))
    o, e, rc = msh_out("if then fi")
    check("syntax error near a token", rc == 2 and "unexpected token" in e, (o, e, rc))
    o, e, rc = msh_out("cat <<EOF\nx\nEOF")
    check("here-documents are refused clearly", rc == 2 and "here-documents" in e, (o, e, rc))
    o, e, rc = msh_out("echo ${x?must be set}; echo no")
    check("${x?} stops the command", rc == 1 and "must be set" in e and "no" not in o, (o, e, rc))
    r = subprocess.run([msh], input="echo from-stdin\nfor i in 1 2\ndo echo $i\ndone\nread v\nhello-read\necho v=$v\n",
                       capture_output=True, text=True, env=env, timeout=20)
    check("commands from a pipe, read gets the rest", r.stdout == "from-stdin\n1\n2\nv=hello-read\n", r.stdout + r.stderr)
    script = tmp / "script.msh"
    script.write_text("#!/usr/bin/msh\necho args: $0 $# $1\nexit 5\n")
    r = subprocess.run([msh, str(script), "a1", "b2"], capture_output=True, text=True, env=env, timeout=20)
    check("script file with arguments", r.stdout == f"args: {script} 2 a1\n" and r.returncode == 5, (r.stdout, r.returncode))
    r = subprocess.run([msh, "-n", "-c", "for x in; do"], capture_output=True, text=True, timeout=20)
    check("-n reports syntax errors without running", r.returncode == 2, r.stderr)
    o, e, rc = msh_out("x=1; alias say='echo said'; say $x")
    check("aliases are only expanded on later lines", rc == 127, (o, e, rc))
    o, e, rc = msh_out("eval \"alias say='echo said'\"; eval 'say it'")
    check("alias defined, then used", o == "said it\n", (o, e, rc))


# --- interactive, on a pseudo-terminal ------------------------------------------------

class Term:
    FIRST = "T> "
    # A new prompt always starts a line; a redrawn line (after an edit)
    # reprints the prompt after "\r\x1b[0K" instead, which this skips.
    PROMPT = "\nT> "

    def __init__(self, msh, home, cols=80, rows=24, answer_cpr=None, env_extra=None):
        env = dict(os.environ, HOME=str(home), TERM="vt100", LC_ALL="C", PATH=os.environ["PATH"])
        env.pop("PROMPT_COMMAND", None)
        env.update(env_extra or {})
        self.answer_cpr = answer_cpr
        pid, fd = pty.fork()
        if pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            os.chdir(home)
            os.execve(msh, [ "-msh" ], env)
        self.pid, self.fd = pid, fd
        self.raw = b""
        self.mark = 0
        self.screen = pyte.Screen(answer_cpr[1] if answer_cpr else cols, rows) if pyte else None
        self.stream = pyte.ByteStream(self.screen) if pyte else None

    def pump(self, timeout):
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return False
        try:
            data = os.read(self.fd, 65536)
        except OSError:
            return False
        if not data:
            return False
        self.raw += data
        if self.stream:
            self.stream.feed(data)
        if self.answer_cpr and b"\x1b[6n" in data:
            os.write(self.fd, b"\x1b[%d;%dR" % self.answer_cpr)
        return True

    def text(self):
        return self.raw.decode(errors="replace")

    def expect(self, want, timeout=5):
        """Wait for `want` (str or regex) after the mark; move the mark past it."""
        end = time.monotonic() + timeout
        while True:
            t = self.text()
            m = re.compile(want).search(t, self.mark) if isinstance(want, re.Pattern) else None
            idx = (m.start() if m else -1) if isinstance(want, re.Pattern) else t.find(want, self.mark)
            if idx >= 0:
                stop = m.end() if m else idx + len(want)
                got = t[self.mark:idx]
                self.mark = stop
                return got
            left = end - time.monotonic()
            if left <= 0:
                raise TimeoutError(f"timed out waiting for {want!r}; got {t[self.mark:][-400:]!r}")
            self.pump(min(left, 0.2))

    def send(self, s):
        os.write(self.fd, s.encode() if isinstance(s, str) else s)

    def run(self, line, timeout=5):
        """Type a line, return its output (without the echoed line)."""
        self.send(line + "\r")
        out = self.expect(self.PROMPT, timeout)  # ends just before the prompt's newline
        out = out.split("\n", 1)[1] + "\n" if "\n" in out else ""
        return out.replace("\r", "")

    def settle(self, t=0.3):
        end = time.monotonic() + t
        while time.monotonic() < end:
            self.pump(0.05)

    def row(self, y):
        return self.screen.display[y].rstrip() if self.screen else None

    def cursor_row_text(self):
        return self.row(self.screen.cursor.y) if self.screen else None

    def close(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        os.close(self.fd)

    def wait_exit(self, timeout=5):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump(0.1)
            p, st = os.waitpid(self.pid, os.WNOHANG)
            if p:
                return os.waitstatus_to_exitcode(st)
        return None


def interactive(msh, tmp):
    home = tmp / "home"
    home.mkdir()
    (home / ".mshrc").write_text("PS1='T> '\nalias hi='echo hello-alias'\n")
    (home / "unique_file_abc").write_text("completed-content\n")
    (home / "ab1").write_text("")
    (home / "ab2").write_text("")
    (home / "a dir").mkdir()
    (home / "a dir" / "inner").write_text("inside\n")

    t = Term(msh, home)
    try:
        t.expect(Term.FIRST)
        check("prompt from ~/.mshrc", True)
        check("argv[0] -msh is a login shell", t.run("echo $0") == "-msh\n")
        check("pipe", t.run("echo hi | tr a-z A-Z") == "HI\n")
        check("alias from ~/.mshrc", t.run("hi") == "hello-alias\n")

        # Line editing: arrows, Home/End, Ctrl-A/E/K/U/Y/W
        t.send("echo abc\x1b[D\x1b[D\x1b[DX\r")
        check("left arrow + insert", "Xabc" in t.expect(Term.PROMPT))
        t.send("cho mid\x01e\x05dle\r")
        check("Ctrl-A, Ctrl-E", "middle" in t.expect(Term.PROMPT))
        t.send("echo keep junk\x17\x17keep2\r")
        check("Ctrl-W kills words", "keep2" in t.expect(Term.PROMPT) and "junk" not in t.text()[t.mark - 20:])
        t.send("garbage\x15echo after-ctrl-u\r")
        check("Ctrl-U kills the line", "after-ctrl-u" in t.expect(Term.PROMPT))
        t.send("echo a\x1b[1~echo \x1b[4~ b\r")
        check("Home/End keys", "echo a b" in t.expect(Term.PROMPT))
        if t.screen:
            check("screen shows the edited line", any("echo a b" in t.row(y) for y in range(24)))

        # History
        t.run("echo first-cmd")
        t.send("\x1b[A\r")
        check("up arrow recalls", "first-cmd" in t.expect(Term.PROMPT))
        t.send("echo !!\r")
        out = t.expect(Term.PROMPT)
        check("!! expands and is shown", "echo echo first-cmd" in out and "first-cmd" in out)
        t.send("\x12first-c")
        t.settle(0.3)
        if t.screen:
            check("Ctrl-R shows the search", "(reverse-i-search)`first-c'" in t.cursor_row_text(), t.cursor_row_text())
        t.send("\r")
        check("Ctrl-R finds and runs", "first-cmd" in t.expect(Term.PROMPT))
        hist = t.run("history")
        check("history lists commands", "echo first-cmd" in hist and "echo hi | tr a-z A-Z" in hist, hist)

        # Completion
        t.send("cat uniq\t\r")
        check("Tab completes a file", "completed-content" in t.expect(Term.PROMPT))
        t.send("ech\t from-completion\r")
        check("Tab completes a command", "from-completion" in t.expect(Term.PROMPT))
        t.send("cat a\\ d\tinn\t\r")
        check("Tab completes into a directory with a space", "inside" in t.expect(Term.PROMPT))
        t.send("ls ab\t\t")
        t.settle(0.5)
        listed = t.text()[t.mark:]
        check("double Tab lists matches", "ab1" in listed and "ab2" in listed, listed)
        t.send("\x15\r")
        t.expect(Term.PROMPT)
        t.send("echo $HO\t\r")
        check("Tab completes variables", str(home) in t.expect(Term.PROMPT))

        # Multi-line
        t.send("for i in 1 2; do\r")
        t.expect("> ")
        t.send("echo L$i\r")
        t.expect("> ")
        t.send("done\r")
        out = t.expect(Term.PROMPT)
        check("multi-line for loop", "L1" in out and "L2" in out, out)
        check("multi-line history entry", "for i in 1 2; do echo L$i; done" in t.run("history 2"))

        # Ctrl-C
        t.send("half typed\x03")
        t.expect(Term.PROMPT)
        check("Ctrl-C at the prompt", t.run("echo $?") == "130\n")
        t.send("while true; do sleep 0.1; done\r")
        t.settle(0.5)
        t.send("\x03")
        t.expect(Term.PROMPT, 5)
        check("Ctrl-C ends a shell loop", t.run("echo $?") == "130\n")
        t.send("i=0; while true; do i=$((i+1)); done\r")
        t.settle(0.5)
        t.send("\x03")
        t.expect(Term.PROMPT, 5)
        check("Ctrl-C ends a loop with no child process", True)

        # Job control
        t.send("sleep 30\r")
        t.settle(0.5)
        t.send("\x1a")
        out = t.expect(Term.PROMPT)
        check("Ctrl-Z stops the job", re.search(r"\[1\]\+\s+Stopped\s+sleep 30", out) is not None, out)
        check("jobs shows it stopped", re.search(r"\[1\]\+\s+Stopped\s+sleep 30", t.run("jobs")) is not None)
        check("bg resumes it", "[1]+ sleep 30 &" in t.run("bg"))
        check("jobs shows it running", re.search(r"\[1\]\+\s+Running\s+sleep 30 &", t.run("jobs")) is not None)
        t.run("kill %1")
        t.settle(0.3)
        out = t.run("true")
        check("finished job is reported", re.search(r"\[1\]\+\s+Terminated\s+sleep 30", out) is not None, out)
        t.run("sleep 30 &")
        t.send("fg\r")
        t.expect("sleep 30")
        t.settle(0.3)
        t.send("\x03")
        t.expect(Term.PROMPT)
        check("fg, then Ctrl-C", t.run("echo $?") == "130\n")
        t.send("sleep 0.3 &\r")
        t.expect(Term.PROMPT)
        time.sleep(0.6)
        out = t.run("echo next")
        check("Done is reported at the next prompt", re.search(r"\[1\]\+\s+Done\s+sleep 0.3", out) is not None, out)
        t.send("cat\r")
        t.settle(0.3)
        t.send("typed-into-cat\r")
        t.expect("typed-into-cat")
        t.expect("typed-into-cat")  # the tty's echo, then cat's copy
        t.send("\x04")
        t.expect(Term.PROMPT)
        check("a foreground program gets the terminal", True)
        out = t.run("sleep 10 | cat & sleep 0.2; jobs -l")
        check("background pipeline is one job", re.search(r"\[1\]\+\s+\d+ Running\s+sleep 10 \| cat &", out) is not None, out)
        t.run("kill %1")
        t.settle(0.3)
        t.run("true")

        # Exit with stopped jobs: warned once
        t.send("sleep 40\r")
        t.settle(0.4)
        t.send("\x1a")
        t.expect(Term.PROMPT)
        t.send("exit\r")
        check("exit warns about stopped jobs", "stopped jobs" in t.expect(Term.PROMPT))
        t.send("\x04")
        code = t.wait_exit()
        check("Ctrl-D exits", code is not None, code)
    except (TimeoutError, OSError) as e:
        check("interactive session", False, e)
    finally:
        t.close()

    # History file survives the session.
    hf = home / ".msh_history"
    check("history file written", hf.exists() and "echo first-cmd" in hf.read_text())
    t = Term(msh, home)
    try:
        t.expect(Term.FIRST)
        t.send("\x1b[A")
        t.settle(0.3)
        check("up arrow reaches the last session", "T> exit" in t.text()[t.mark:], t.text()[-80:])
        t.send("\x15")
        check("history from the last session", "echo first-cmd" in t.run("history 50"))
    except TimeoutError as e:
        check("second session", False, e)
    finally:
        t.close()

    # A serial console: the kernel reports 0 columns, so msh asks the
    # terminal and passes the answer on.
    t = Term(msh, home, cols=0, rows=0, answer_cpr=(40, 132))
    try:
        t.expect(Term.FIRST)
        out = t.run("stty size")
        check("size from a cursor position report", out.strip() == "40 132", out)
    except TimeoutError as e:
        check("serial console size", False, e)
    finally:
        t.close()

    # Without an answer the shell still starts (the boot test's console).
    t = Term(msh, home, cols=0, rows=0)
    try:
        t0 = time.monotonic()
        t.expect(Term.FIRST)
        check("no size reply: prompt within 1 s", time.monotonic() - t0 < 1.0)
        long = "echo " + "x" * 120 + " end-of-long"
        out = t.run(long)
        check("a long line runs as typed", out == "x" * 120 + " end-of-long\n", out)
        check("echo of a long line has no extra newline", "\n" not in t.text().rsplit(long[:20], 1)[-1].split("end-of-long", 1)[0])
    except TimeoutError as e:
        check("console without size", False, e)
    finally:
        t.close()

    # Editing a line that wraps (40 columns): what's on screen must match.
    if pyte:
        t = Term(msh, home, cols=40)
        try:
            t.expect(Term.FIRST)
            base = "echo " + "abcdefghij" * 5  # wraps onto a second row
            t.send(base)
            t.settle(0.2)
            t.send("\x01")              # Ctrl-A: back to the first row
            t.send("\x1b[C" * 5 + "ZZ ")   # after "echo ", insert "ZZ "
            t.settle(0.3)
            rows = [t.row(y) for y in range(24)]
            joined = "".join(r for r in rows if r)
            check("wrapped line redrawn correctly", ("T> echo ZZ " + "abcdefghij" * 5) in joined, rows[:6])
            check("cursor where the edit was", t.screen.cursor.x == len("T> echo ZZ ") and
                  rows[t.screen.cursor.y].startswith("T> echo ZZ"), (t.screen.cursor.x, t.screen.cursor.y))
            t.send("\x05\r")
            out = t.expect(Term.PROMPT)
            check("wrapped edited line runs", ("ZZ " + "abcdefghij" * 5) in out.replace("\r\n", "").replace("\n", ""), out)
            visible = [r for r in (t.row(y) for y in range(24)) if r]
            check("no leftovers on screen", sum("T> echo ZZ" in r for r in visible) == 1, visible)
        except TimeoutError as e:
            check("wrapped editing", False, e)
        finally:
            t.close()
    else:
        print("SKIP  screen checks: pip install pyte")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--msh", help="msh binary to test (default: build cpp/msh)")
    args = ap.parse_args()
    msh = args.msh
    if not msh:
        build = pathlib.Path(tempfile.gettempdir()) / "msh-test-build"
        subprocess.run(["cmake", "-S", ROOT / "cpp/msh", "-B", build, "-DCMAKE_BUILD_TYPE=Release"], check=True, stdout=subprocess.DEVNULL)
        subprocess.run(["cmake", "--build", build, "-j"], check=True, stdout=subprocess.DEVNULL)
        msh = str(build / "msh")
    if not shutil.which("bash"):
        sys.exit("needs bash, to compare script behaviour against")
    with tempfile.TemporaryDirectory() as d:
        tmp = pathlib.Path(d)
        run_scripts(msh, tmp)
        interactive(msh, tmp)
    failed = results.count(False)
    print(f"\n{len(results) - failed}/{len(results)} msh checks passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
