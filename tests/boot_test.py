#!/usr/bin/env python3
"""Phase 0 boot test.

Boots the image in QEMU, waits for the login prompt (the Phase 0 "done"
criterion), logs in, runs each language's smoke test, then powers off.
Exits non-zero on any failure, so it can run in CI as-is.

Usage: python3 tests/boot_test.py [--timeout SECONDS]
"""
import argparse
import os
import pathlib
import select
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROMPT = "# "

# (command, text that must appear in its output)
CHECKS = [
    ("uname -sr", "Linux 6.18"),
    ("hostname", "myos"),
    ("myos-hello-rust", "hello from Rust on x86_64-linux"),
    ("myos-hello-cpp", "hello from C++17 on x86_64"),
    ("myos-hello-py", "hello from Python 3."),
    ("mount | grep -c -E ' /(proc|sys|dev) '", "3"),
]


class Console:
    def __init__(self, proc, log):
        self.proc = proc
        self.log = log
        self.buf = ""

    def expect(self, text, timeout):
        deadline = time.monotonic() + timeout
        while text not in self.buf:
            left = deadline - time.monotonic()
            if left <= 0:
                raise TimeoutError(f"timed out waiting for {text!r}")
            if self.proc.poll() is not None:
                raise RuntimeError(f"QEMU exited ({self.proc.returncode}) waiting for {text!r}")
            ready, _, _ = select.select([self.proc.stdout], [], [], min(left, 0.5))
            if ready:
                chunk = os.read(self.proc.stdout.fileno(), 4096).decode(errors="replace")
                self.log.write(chunk)
                self.log.flush()
                self.buf += chunk
        before, _, self.buf = self.buf.partition(text)
        return before

    def send(self, line):
        self.proc.stdin.write((line + "\n").encode())
        self.proc.stdin.flush()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--timeout", type=int, default=180, help="seconds to wait for login prompt")
    args = ap.parse_args()

    log_path = ROOT / "output" / "boot_test.log"
    log_path.parent.mkdir(exist_ok=True)
    proc = subprocess.Popen(
        [str(ROOT / "scripts" / "run-qemu.sh")],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    failures = []
    with open(log_path, "w") as log:
        con = Console(proc, log)
        try:
            t0 = time.monotonic()
            con.expect("login:", args.timeout)
            print(f"PASS  reached login prompt in {time.monotonic() - t0:.1f}s")

            con.send("root")
            con.expect("Password:", 10)
            con.send("myos")
            con.expect(PROMPT, 15)

            for cmd, want in CHECKS:
                con.send(cmd)
                out = con.expect(PROMPT, 30)
                ok = want in out
                print(f"{'PASS' if ok else 'FAIL'}  {cmd}")
                if not ok:
                    failures.append(cmd)
                    print(f"      expected {want!r}, got: {out.strip()!r}")

            con.send("poweroff")
            proc.wait(timeout=30)
        except Exception as e:  # noqa: BLE001 - report anything as a test failure
            failures.append(str(e))
            print(f"FAIL  {e}")
        finally:
            if proc.poll() is None:
                proc.kill()

    print(f"\nconsole log: {log_path}")
    if failures:
        print(f"{len(failures)} check(s) failed")
        return 1
    print("Phase 0 boot test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
