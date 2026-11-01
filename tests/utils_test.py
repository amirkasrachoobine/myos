#!/usr/bin/env python3
"""Host-side tests for the Phase 4 utilities: no Buildroot build, no QEMU.

  ls, cat   compared byte for byte with GNU coreutils on a test directory
            (regular files, directories, links, odd names, control bytes).
  ps, top   run against busd and sysinfod on a private bus socket, checked
            against /proc; top also interactively on a pseudo-terminal, and
            suspended with Ctrl-Z and resumed with fg under msh. Their errors
            when sysinfod or busd is down are checked too.

Usage: python3 tests/utils_test.py [--no-build]
  builds cpp/utils (with cpp/bus) and cpp/msh into /tmp, and rust/bus and
  rust/services with cargo, unless --no-build
"""
import argparse
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from msh_test import Term, pyte  # noqa: E402  (pseudo-terminal helper)

ROOT = pathlib.Path(__file__).resolve().parent.parent
UTILS = pathlib.Path(tempfile.gettempdir()) / "myos-utils-build"
MSH = pathlib.Path(tempfile.gettempdir()) / "msh-test-build"
BUSD = ROOT / "rust/bus/target/release/busd"
SYSINFOD = ROOT / "rust/services/target/release/sysinfod"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + ("" if ok or not detail else "\n      " + str(detail).strip().replace("\n", "\n      ")))


def build():
    for src, out in ((ROOT / "cpp/utils", UTILS), (ROOT / "cpp/msh", MSH)):
        subprocess.run(["cmake", "-S", src, "-B", out, "-DCMAKE_BUILD_TYPE=Release"], check=True, stdout=subprocess.DEVNULL)
        subprocess.run(["cmake", "--build", out, "-j"], check=True, stdout=subprocess.DEVNULL)
    for d in ("rust/bus", "rust/services"):
        subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT / d, check=True)


def make_tree(d):
    (d / "plain.txt").write_text("hello\n")
    (d / "big.bin").write_bytes(b"x" * 300000)
    (d / "empty").write_text("")
    (d / "run.sh").write_text("#!/bin/sh\n")
    os.chmod(d / "run.sh", 0o755)
    (d / "sub").mkdir()
    (d / "sub" / "deep").mkdir()
    (d / "sub" / "deep" / "f").write_text("f")
    (d / "sub" / "g").write_text("g")
    (d / ".hidden").write_text("h")
    (d / "with space").write_text("s")
    (d / "Upper").write_text("u")
    os.symlink("plain.txt", d / "link")
    os.symlink("nowhere", d / "broken")
    os.symlink("sub", d / "dirlink")
    os.mkfifo(d / "fifo")
    for i in range(30):
        (d / f"n{i:02d}_{'x' * (i % 7)}").write_text("")
    # Fixed times, so -t is deterministic and dates print the same.
    for i, p in enumerate(sorted(d.rglob("*"))):
        if not p.is_symlink():
            os.utime(p, (1700000000 + i * 3600, 1700000000 + i * 3600))
    os.utime(d / "plain.txt", (time.time() - 60, time.time() - 60))  # recent: shows a time, not a year


def compare(name, ours, gnu, cwd, env, stdin=None):
    a = subprocess.run(ours, capture_output=True, cwd=cwd, env=env, input=stdin)
    b = subprocess.run(gnu, capture_output=True, cwd=cwd, env=env, input=stdin)
    ok = a.stdout == b.stdout and a.returncode == b.returncode
    detail = ""
    if not ok:
        detail = f"ours ({a.returncode}):\n{a.stdout.decode(errors='replace')}\nGNU ({b.returncode}):\n{b.stdout.decode(errors='replace')}"
    check(name, ok, detail)


def test_ls_cat(tmp):
    d = tmp / "tree"
    d.mkdir()
    make_tree(d)
    env = dict(os.environ, LC_ALL="C", TZ="UTC", COLUMNS="80")
    gnu_ls = shutil.which("ls")
    for flags in ([], ["-a"], ["-A"], ["-1"], ["-l"], ["-la"], ["-lh"], ["-ln"], ["-lt"], ["-lS"], ["-lr"],
                  ["-F"], ["-p"], ["-d", "sub", "plain.txt"], ["-R", "sub"], ["-i", "-1"], ["-C"],
                  ["plain.txt", "sub", "link"], ["-l", "link", "dirlink", "broken"], ["dirlink"], ["-1t"],
                  ["-lU", "sub"], ["--color=never"], ["-C", "sub", "."]):
        # -T 0: GNU pads columns with tabs by default; ours pads with spaces.
        compare(f"ls {' '.join(flags)}", [str(UTILS / "ls"), *flags], [gnu_ls, "-N", "-T", "0", *flags], d, env)
    envw = dict(env, COLUMNS="40")
    compare("ls -C at 40 columns", [str(UTILS / "ls"), "-C"], [gnu_ls, "-N", "-T", "0", "-C"], d, envw)
    r = subprocess.run([UTILS / "ls", "--color=always", "-1", "sub", "run.sh", "link", "broken"], capture_output=True, text=True, cwd=d, env=env)
    check("ls colours: dir, executable, link, broken link",
          all(x in r.stdout for x in ("\x1b[01;32mrun.sh", "\x1b[01;36mlink", "\x1b[01;31mbroken", "\x1b[01;34mdeep")), r.stdout)
    r = subprocess.run([UTILS / "ls", "missing"], capture_output=True, text=True, cwd=d, env=env)
    check("ls missing: status 2 and a message", r.returncode == 2 and "cannot access 'missing'" in r.stderr, r)

    data = d / "data"
    data.write_bytes(b"one\ttab\n\n\n\nafter blanks\nctrl\x01\x7f high\xe9\xff\nno newline")
    other = d / "other"
    other.write_bytes(b"second\n\nfile\n")
    gnu_cat = shutil.which("cat")
    for flags in ([], ["-n"], ["-b"], ["-s"], ["-E"], ["-T"], ["-v"], ["-A"], ["-e"], ["-t"], ["-ns"], ["-bE"]):
        compare(f"cat {' '.join(flags) or '(plain)'}", [str(UTILS / "cat"), *flags, "data", "other"],
                [gnu_cat, *flags, "data", "other"], d, env)
    compare("cat - (stdin) between files", [str(UTILS / "cat"), "-n", "other", "-", "other"],
            [gnu_cat, "-n", "other", "-", "other"], d, env, stdin=b"from stdin\n")
    r = subprocess.run([UTILS / "cat", "nope", "other"], capture_output=True, text=True, cwd=d, env=env)
    check("cat missing file: message, carries on, status 1",
          r.returncode == 1 and "cat: nope: No such file or directory" in r.stderr and r.stdout == "second\n\nfile\n", r)
    r = subprocess.run([UTILS / "cat", "sub"], capture_output=True, text=True, cwd=d, env=env)
    check("cat a directory", r.returncode == 1 and "Is a directory" in r.stderr, r)
    big = d / "big.bin"
    r = subprocess.run([UTILS / "cat", "big.bin"], capture_output=True, cwd=d)
    check("cat a 300 KB file", r.stdout == big.read_bytes())
    p = subprocess.run(f"{UTILS / 'cat'} big.bin big.bin | head -c 10", shell=True, capture_output=True, cwd=d)
    check("cat into a closed pipe exits quietly", p.stdout == b"x" * 10 and p.stderr == b"", p.stderr)


class Services:
    def __init__(self, tmp):
        self.sock = tmp / "bus.sock"
        self.env = dict(os.environ, MYOS_BUS_SOCKET=str(self.sock), LC_ALL="C")
        self.procs = {}

    def start(self, name, cmd):
        self.procs[name] = subprocess.Popen(cmd, env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                            start_new_session=True)

    def stop(self, name):
        p = self.procs.pop(name, None)
        if p and p.poll() is None:
            p.terminate()
            p.wait(5)

    def up(self):
        self.start("busd", [str(BUSD), "--socket", str(self.sock)])
        for _ in range(50):
            if self.sock.exists():
                break
            time.sleep(0.1)
        self.start("sysinfod", [str(SYSINFOD)])
        for _ in range(50):
            r = subprocess.run([UTILS / "ps", "-p", "1"], env=self.env, capture_output=True)
            if r.returncode == 0:
                break
            time.sleep(0.1)
        time.sleep(1.2)  # one CPU sample

    def down(self):
        for n in list(self.procs):
            self.stop(n)


def proc_pids():
    return {int(p) for p in os.listdir("/proc") if p.isdigit()}


def test_ps_top(tmp):
    svc = Services(tmp)
    env = svc.env
    try:
        svc.up()
        ps = str(UTILS / "ps")

        def run(*a, **kw):
            return subprocess.run([ps, *a], capture_output=True, text=True, env=env, **kw)

        r = run()
        lines = r.stdout.splitlines()
        check("ps: header", lines and lines[0].split() == ["PID", "USER", "TIME", "COMMAND"], lines[:1])
        listed = {int(l.split()[0]) for l in lines[1:]}
        before = proc_pids()
        missing = {p for p in before if p not in listed and os.path.exists(f"/proc/{p}")}
        check("ps: every process in /proc", len(missing) <= 2, missing)
        me = os.getpid()
        r = run("-p", str(me), "-o", "pid,ppid,user,comm,nlwp")
        row = r.stdout.splitlines()[-1].split()
        import pwd
        check("ps -p -o: right values", row == [str(me), str(os.getppid()), pwd.getpwuid(os.getuid()).pw_name,
                                               open(f"/proc/{me}/comm").read().strip(), str(len(os.listdir(f'/proc/{me}/task')))], row)
        r = run("aux")
        check("ps aux: header", r.stdout.split("\n")[0].split() ==
              ["USER", "PID", "%CPU", "%MEM", "VSZ", "RSS", "STAT", "START", "TIME", "COMMAND"])
        check("ps aux: busd's command line", re.search(rf"\S+\s+{svc.procs['busd'].pid}\s.*busd --socket", r.stdout) is not None)
        r = run("-ef")
        check("ps -ef: full format", r.stdout.split("\n")[0].split() == ["USER", "PID", "PPID", "%CPU", "START", "TIME", "COMMAND"])
        r = run("-C", "sysinfod,busd", "-o", "comm=")
        check("ps -C, header-less -o", sorted(r.stdout.split()) == ["busd", "sysinfod"], r.stdout)
        r = run("--sort=-rss", "-o", "rss=")
        vals = [int(x) for x in r.stdout.split()]
        check("ps --sort=-rss", vals == sorted(vals, reverse=True) and len(vals) > 3)
        r = run("-u", "root", "-o", "user=")
        check("ps -u root", set(r.stdout.split()) == {"root"}, set(r.stdout.split()))
        r = run("-p", "999999999")
        check("ps -p with no match: status 1", r.returncode == 1)
        r = run("-o", "bogus")
        check("ps -o bogus: usage error", r.returncode == 2 and "unknown field" in r.stderr)
        # kernel threads in brackets
        r = run("-p", "2", "-o", "args=")
        check("ps: kernel threads shown as [name]", r.stdout.strip() == "[kthreadd]" or not os.path.exists("/proc/2/comm"), r.stdout)

        top = str(UTILS / "top")
        r = subprocess.run([top, "-b", "-n", "2", "-d", "0.3"], capture_output=True, text=True, env=env, timeout=10)
        frames = r.stdout.count("top - ")
        check("top -b -n 2: two frames", frames == 2 and r.returncode == 0, r.stdout[:300] + r.stderr)
        first = r.stdout.split("\n\n")[0]
        m = re.search(r"Tasks:\s+(\d+) total", first)
        check("top: task count matches /proc", m and abs(int(m.group(1)) - len(proc_pids())) <= 5, m and m.group(0))
        check("top: memory line", re.search(r"MiB Mem :\s+[\d.]+ total", first) is not None)
        r = subprocess.run([top, "-b", "-n", "1", "-o", "mem"], capture_output=True, text=True, env=env)
        rows = [l.split() for l in r.stdout.splitlines() if re.match(r"\s*\d+ ", l)]
        mems = [float(x[7]) for x in rows]
        check("top -o mem sorts by %MEM", mems == sorted(mems, reverse=True), mems[:5])
        r = subprocess.run([top, "-b", "-n", "1", "-p", str(me)], capture_output=True, text=True, env=env)
        rows = [l for l in r.stdout.splitlines() if re.match(r"\s*\d+ ", l)]
        check("top -p shows one process", len(rows) == 1 and rows[0].split()[0] == str(me), rows)

        if pyte:
            t = Term(str(UTILS / "top"), tmp, cols=100, rows=30, env_extra={"MYOS_BUS_SOCKET": str(svc.sock)})
            try:
                t.expect("Tasks:")
                t.settle(0.5)
                check("top on a terminal draws its screen", t.row(0).startswith("top - ") and "PID USER" in t.row(6), [t.row(i) for i in range(9)])
                t.send("M")
                t.settle(0.8)
                rows = [t.row(y).split() for y in range(8, 30) if re.match(r"\s*\d+ ", t.row(y) or "")]
                mems = [float(r[7]) for r in rows]
                check("M sorts by memory", mems == sorted(mems, reverse=True) and len(mems) > 5, mems)
                t.send("h")
                t.settle(0.4)
                check("h shows help", any("sort by %CPU" in (t.row(y) or "") for y in range(30)))
                t.send(" ")
                t.settle(0.4)
                t.send("q")
                code = t.wait_exit(5)
                check("q quits", code == 0, code)
                check("terminal restored (normal screen, cursor on)", t.raw.endswith(b"\x1b[?25h\x1b[?1049l"), t.raw[-40:])
            finally:
                t.close()

            # Ctrl-Z and fg from msh: top must give the terminal back and take it again.
            home = tmp / "mshhome"
            home.mkdir(exist_ok=True)
            (home / ".mshrc").write_text(f"PS1='T> '\nPATH={UTILS}:$PATH\n")
            t = Term(str(MSH / "msh"), home, cols=100, rows=30, env_extra={"MYOS_BUS_SOCKET": str(svc.sock)})
            try:
                t.expect(Term.FIRST)
                t.send("top -d 0.5\r")
                t.expect("Tasks:")
                t.send("\x1a")
                out = t.expect(Term.PROMPT)
                check("Ctrl-Z stops top", re.search(r"\[1\]\+\s+Stopped\s+top -d 0.5", out) is not None, out)
                check("the shell works while top is stopped", t.run("echo alive") == "alive\n")
                t.send("fg\r")
                t.expect("Tasks:")
                t.settle(0.6)
                check("fg brings top back", t.row(0).startswith("top - "), t.row(0))
                t.send("q")
                t.expect("\x1b[?1049l" + Term.FIRST)  # leaving the alternate screen, then the prompt
                check("q, back at the prompt", t.run("echo $?") == "0\n")
            except TimeoutError as e:
                check("top under msh", False, e)
            finally:
                t.close()
        else:
            print("SKIP  interactive top: pip install pyte")

        if pyte:
            # top keeps going while sysinfod restarts.
            t = Term(str(UTILS / "top"), tmp, cols=120, rows=30, env_extra={"MYOS_BUS_SOCKET": str(svc.sock)})
            try:
                t.send("d")
                t.send("0.3\r")
                t.expect("Tasks:")
                svc.stop("sysinfod")
                t.settle(1.0)
                check("top shows sysinfod is gone", "sysinfod isn't on the bus" in t.row(5), t.row(5))
                svc.start("sysinfod", [str(SYSINFOD)])
                t.settle(2.0)
                check("top recovers when sysinfod is back", t.row(5) == "" and t.row(0).startswith("top - "), [t.row(0), t.row(5)])
                t.send("q")
                check("top still quits", t.wait_exit(5) == 0)
            finally:
                t.close()

        # Failures: sysinfod gone, then busd gone.
        svc.stop("sysinfod")
        time.sleep(0.3)
        r = run()
        check("sysinfod down: ps says so", r.returncode == 1 and "sysinfod isn't on the bus" in r.stderr, r.stderr)
        r = subprocess.run([top, "-b", "-n", "1"], capture_output=True, text=True, env=env)
        check("sysinfod down: top -b says so", r.returncode == 1 and "sysinfod" in r.stderr, r.stderr)
        svc.stop("busd")
        time.sleep(0.3)
        r = run()
        check("busd down: ps says so", r.returncode == 1 and "can't reach the bus" in r.stderr, r.stderr)
    finally:
        svc.down()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--no-build", action="store_true")
    args = ap.parse_args()
    if not args.no_build:
        build()
    with tempfile.TemporaryDirectory() as d:
        tmp = pathlib.Path(d)
        test_ls_cat(tmp)
        test_ps_top(tmp)
    failed = results.count(False)
    print(f"\n{len(results) - failed}/{len(results)} utility checks passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
