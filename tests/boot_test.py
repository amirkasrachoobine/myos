#!/usr/bin/env python3
"""Boot test for the myos image.

Boots the image in QEMU, waits for the login prompt, logs in, runs each
check below, then powers off and checks the shutdown was clean.
Exits non-zero on any failure, so it can run in CI as-is.

Phase 0: kernel, hostname, one hello program per language, API filesystems.
Phase 1: PID 1 is myos-init; services start, restart, stop and start again;
         orphans are reaped; `poweroff` shuts the machine down cleanly.
Phase 2: busd runs under init; the Rust, C++ and Python clients (and Java,
         with WITH_JAVA=1) ping it; a killed busd is restarted.

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

# Oneshots run in the background after boot; give them up to 20s.
WAIT_HELLO = "for i in $(seq 20); do initctl status hello | grep -q done && break; sleep 1; done; initctl status hello"
ZOMBIES = "sh -c 'sleep 0.2 & exit'; sleep 1; echo zombies=$(grep -l '^State:.*Z' /proc/[0-9]*/status 2>/dev/null | wc -l)"

# (command, text that must appear in its output)
CHECKS = [
    # Phase 0
    ("uname -sr", "Linux 6.18"),
    ("hostname", "myos"),
    ("myos-hello-rust", "hello from Rust on x86_64-linux"),
    ("myos-hello-cpp", "hello from C++17 on x86_64"),
    ("myos-hello-py", "hello from Python 3."),
    ("mount | grep -c -E ' /(proc|sys|dev) '", "3"),
    # Phase 1
    ("readlink /proc/1/exe", "/usr/bin/myos-init"),
    ("initctl status getty", "running"),
    ("initctl status syslogd", "running"),
    ("initctl status klogd", "running"),
    (WAIT_HELLO, "done      exit code 0"),
    ("kill -9 $(pidof syslogd); sleep 2; initctl status syslogd", "restarts 1"),
    ("initctl stop syslogd >/dev/null; sleep 1; initctl status syslogd", "stopped"),
    ("initctl start syslogd >/dev/null; sleep 1; initctl status syslogd", "running"),
    (ZOMBIES, "zombies=0"),
    # Phase 2
    ("initctl status busd", "running"),
    ("myos-bus ping", 'pong: "hello from Rust"'),
    ("myos-bus-ping-cpp", 'pong: "hello from C++"'),
    ("myos-bus-ping-py", 'pong: "hello from Python"'),
    ("myos-bus-selftest", "clients ok"),
    ("echo listed=$(myos-bus list | grep -c '^bus ')", "listed=1"),
    ("kill $(pidof busd); sleep 2; myos-bus ping back", 'pong: "back"'),
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

    def drain(self, timeout):
        """Read until QEMU exits or `timeout` passes; return everything read."""
        deadline = time.monotonic() + timeout
        while self.proc.poll() is None and time.monotonic() < deadline:
            ready, _, _ = select.select([self.proc.stdout], [], [], 0.5)
            if ready:
                chunk = os.read(self.proc.stdout.fileno(), 4096).decode(errors="replace")
                if not chunk:
                    break
                self.log.write(chunk)
                self.log.flush()
                self.buf += chunk
        return self.buf

    def send(self, line):
        self.proc.stdin.write((line + "\n").encode())
        self.proc.stdin.flush()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--timeout", type=int, default=180, help="seconds to wait for login prompt")
    args = ap.parse_args()

    out_dir = pathlib.Path(os.environ.get("O", ROOT / "output"))
    out_dir.mkdir(exist_ok=True)
    log_path = out_dir / "boot_test.log"
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
                out = con.expect(PROMPT, 40)
                # The console echoes the command line itself; only look at what follows it.
                out = out.split("\n", 1)[1] if "\n" in out else ""
                ok = want in out
                label = "wait for hello oneshot" if cmd is WAIT_HELLO else "reap orphans" if cmd is ZOMBIES else cmd
                print(f"{'PASS' if ok else 'FAIL'}  {label}")
                if not ok:
                    failures.append(label)
                    print(f"      expected {want!r} in:\n      " + out.strip().replace("\n", "\n      "))

            # Phase 1 done-check: BusyBox poweroff signals our PID 1, which stops
            # the services, syncs, remounts / read-only and powers the VM off.
            con.send("poweroff")
            text = con.drain(30)
            for label, ok in [
                ("init stopped services", "[init] powering off: stopping services" in text),
                ("init synced filesystems", "[init] syncing filesystems" in text),
                ("kernel powered down", "reboot: Power down" in text),
                ("QEMU exited", proc.poll() is not None),
            ]:
                print(f"{'PASS' if ok else 'FAIL'}  poweroff: {label}")
                if not ok:
                    failures.append(f"poweroff: {label}")
        except (TimeoutError, RuntimeError) as e:
            failures.append(str(e))
            print(f"FAIL  {e}")
        finally:
            if proc.poll() is None:
                proc.kill()
            proc.wait()

    print(f"\nconsole log: {log_path}")
    if failures:
        print(f"Boot test FAILED ({len(failures)} failure(s))")
        return 1
    print("Phase 1 boot test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
