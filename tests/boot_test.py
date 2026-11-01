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
Phase 3: netd brings eth0 up with DHCP (QEMU's 10.0.2.15) and writes
         resolv.conf; logd collects service output, syslog, kernel messages
         and bus writes, in memory and in /var/log/messages, and survives a
         restart; sysinfod answers over the bus.
Phase 4: root logs in to msh, so every check above runs through it; then
         pipes, redirection, a multi-line loop, Tab completion, history,
         job control (Ctrl-Z, jobs, bg, kill %1) and Ctrl-C on the serial
         console, and the C++ ls, cat, ps and top in place of BusyBox's.

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
    ("initctl status logd", "running"),
    (WAIT_HELLO, "done      exit code 0"),
    ("kill -9 $(pidof sysinfod); sleep 2; initctl status sysinfo", "restarts 1"),
    ("initctl stop sysinfo >/dev/null; sleep 1; initctl status sysinfo", "stopped"),
    ("initctl start sysinfo >/dev/null; sleep 1; initctl status sysinfo", "running"),
    (ZOMBIES, "zombies=0"),
    # Phase 2
    ("initctl status busd", "running"),
    ("myos-bus ping", 'pong: "hello from Rust"'),
    ("myos-bus-ping-cpp", 'pong: "hello from C++"'),
    ("myos-bus-ping-py", 'pong: "hello from Python"'),
    ("myos-bus-selftest", "clients ok"),
    ("echo listed=$(myos-bus list | grep -c '^bus ')", "listed=1"),
    ("kill $(pidof busd); sleep 2; myos-bus ping back", 'pong: "back"'),
    # Phase 3: networking (QEMU user networking: DHCP from 10.0.2.2, DNS 10.0.2.3)
    ("initctl status netd", "running"),
    ("initctl status network", "done      exit code 0"),
    ("myos-net status", "10.0.2.15/24"),
    ("myos-net status | tail -1", "online"),
    ("grep nameserver /etc/resolv.conf", "nameserver 10.0.2.3"),
    (r"grep -c '^eth0\s00000000\s0202000A' /proc/net/route", "1"),
    # Phase 3: logs
    ("myos-log -t hello", "hello from Rust on x86_64-linux"),
    ("myos-log -t init -g 'started netd'", "started netd"),
    ("myos-log -t netd", "eth0: 10.0.2.15/24 from 10.0.2.2"),
    ("echo kernel=$(myos-log -t kernel -n 1000 | grep -c 'Linux version')", "kernel=1"),
    ("logger -t phase3 via-syslog; sleep 0.5; myos-log -t phase3", "via-syslog"),
    ("myos-log write -t phase3 -p warning via-bus >/dev/null; myos-log -t phase3 -p warning", "warning phase3"),
    # The disk keeps its log across boots, so earlier runs' lines are there too.
    (r"grep -q 'netd\[[0-9]*\]: eth0: 10.0.2.15/24' /var/log/messages && echo in-file", "in-file"),
    ("kill $(pidof logd); sleep 2; logger -t phase3 after-restart; sleep 0.5; myos-log -t phase3", "after-restart"),
    ("echo dupes=$(myos-log -t kernel -n 1000 | grep -c 'Linux version')", "dupes=0"),
    # Phase 3: system information
    ("echo services=$(myos-bus list | grep -c -E '^(log|net|sysinfo) ')", "services=3"),
    ("myos-sysinfo", "memory"),
    ("myos-sysinfo ps", "/usr/bin/logd"),
    ("myos-sysinfo disks", "ext4"),
    # Phase 4: msh is the login shell
    ("readlink /proc/$$/exe", "/usr/bin/msh"),
    ("grep '^root:' /etc/passwd", ":/root:/usr/bin/msh"),
    ("echo $0 $((6 * 7))", "-msh 42"),
    ("echo phase4 | cat -n > /tmp/p4 && cat /tmp/p4", "     1\tphase4"),
    ("ls /nonexistent 2>/dev/null || echo status=$?", "status=2"),
    ("f() { echo \"fn:$1\"; }; f arg", "fn:arg"),
    ("type cd ll", "ll is aliased to `ls -la'"),
    # Phase 4: utilities (ours, not BusyBox's)
    ("for t in ls cat ps top; do $t --version; done | grep -c '(myos)'", "4"),
    ("ls -l /etc/myos/services/getty.toml", "-rw-r--r--"),
    ("ls -a /etc/myos", "services"),
    ("ps -o args= -C logd", "/usr/bin/logd"),
    ("ps -p 1 -o pid= -o comm=", "1 myos-init"),
    ("ps aux | head -1", "%CPU %MEM"),
    ("top -b -n 1 | head -2 | tail -1", "Tasks:"),
    ("echo services=$(top -b -n 1 | grep -c -E ' (busd|logd|netd|sysinfod)$')", "services=4"),
]

# Typed with control keys: (what to send, text that must appear before the
# next prompt). "\x1a" is Ctrl-Z, "\x03" Ctrl-C, "\t" Tab.
INTERACTIVE = [
    ("multi-line for loop", ["for i in 1 2; do\necho loop$i\ndone"], "loop2"),
    ("Tab completes a file name", ["echo completed-ok > /tmp/msh-complete-test", "cat /tmp/msh-compl\t"], "completed-ok"),
    ("history has the completed line", ["history 3"], "cat /tmp/msh-complete-test"),
    ("Ctrl-Z stops a job", ["sleep 30", "<1>\x1a"], "Stopped                 sleep 30"),
    ("jobs lists it", ["jobs"], "[1]+  Stopped                 sleep 30"),
    ("bg resumes it", ["bg"], "[1]+ sleep 30 &"),
    ("kill %1 ends it", ["kill %1; sleep 0.5; jobs"], "Terminated              sleep 30"),
    ("Ctrl-C interrupts", ["sleep 30", "<1>\x03", "echo status=$?"], "status=130"),
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

    def send_raw(self, data):
        self.proc.stdin.write(data.encode())
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

            # Phase 4: control keys. A key starting with "<1>" means: wait a
            # second (the command before it is running), then send the rest
            # without a newline, as a key press.
            for label, keys, want in INTERACTIVE:
                out = ""
                for i, k in enumerate(keys):
                    if k.startswith("<1>"):
                        time.sleep(1)
                        con.send_raw(k[3:])
                    else:
                        con.send(k)
                    if i + 1 < len(keys) and keys[i + 1].startswith("<1>"):
                        continue  # still running: the key comes next
                    out += con.expect(PROMPT, 40)
                ok = want in out
                print(f"{'PASS' if ok else 'FAIL'}  {label}")
                if not ok:
                    failures.append(label)
                    print(f"      expected {want!r} in:\n      " + out.strip().replace("\n", "\n      "))

            # Phase 1 done-check: BusyBox poweroff signals our PID 1, which stops
            # the services, syncs, remounts / read-only and powers the VM off.
            con.send("poweroff")
            text = con.drain(30)
            try:
                proc.wait(timeout=10)  # QEMU closes its output a moment before it exits
            except subprocess.TimeoutExpired:
                pass
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
    print("Boot test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
