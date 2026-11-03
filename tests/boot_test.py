#!/usr/bin/env python3
"""Boot test for the myos image.

Boots the image in QEMU, waits for the login prompt, logs in, runs each
check below, then powers off and checks the shutdown was clean.
Exits non-zero on any failure, so it can run in CI as-is.

Phase 0: kernel, hostname, one hello program per language, API filesystems.
Phase 1: PID 1 is myos-init; services start, restart, stop and start again;
         orphans are reaped; `poweroff` shuts the machine down cleanly.
Phase 2: busd runs under init; the Rust, C++, Python and Java clients
         ping it; a killed busd is restarted.
Phase 3: netd brings eth0 up with DHCP (QEMU's 10.0.2.15) and writes
         resolv.conf; logd collects service output, syslog, kernel messages
         and bus writes, in memory and in /var/log/messages, and survives a
         restart; sysinfod answers over the bus.
Phase 4: root logs in to msh, so every check above runs through it; then
         pipes, redirection, a multi-line loop, Tab completion, history,
         job control (Ctrl-Z, jobs, bg, kill %1) and Ctrl-C on the serial
         console, and the C++ ls, cat, ps and top in place of BusyBox's.
Phase 5: myctl manages services, shows logs and changes settings; then
         myos-install installs the system onto a second, empty virtual
         disk, and a second boot from that disk alone (BIOS, GRUB, root by
         PARTUUID) reaches the login prompt with every service running.
Phase 6: the trimmed JDK; jrun installs the sample app, which runs as the
         service app.org.myos.hello under its own user, without network,
         and logs through logd (the done-check); it's still installed and
         running on the installed disk. Skipped for a WITH_JAVA=0 image.

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
    # comm is the name PID 1 was started as: the kernel runs /sbin/init
    ("ps -p 1 -o pid= -o comm=", "1 init"),
    ("ps aux | head -1", "%CPU %MEM"),
    ("top -b -n 1 | head -2 | tail -1", "Tasks:"),
    ("echo services=$(top -b -n 1 | grep -c -E ' (busd|logd|netd|sysinfod)$')", "services=4"),
    # Phase 5: myctl, services
    ("myctl status | head -1", "SERVICE"),
    ("echo up=$(myctl status | grep -c -E '^(busd|logd|netd|sysinfo|getty) +running ')", "up=5"),
    ("myctl status getty", "Serial console login"),
    ("myctl status nosuch; echo rc=$?", "rc=1"),
    ("myctl restart sysinfo", "running   pid"),
    ("myctl disable hello", "disabled"),
    ("grep '^enabled' /etc/myos/services/hello.toml", "enabled = false"),
    ("myctl enable hello", "done      exit code 0"),
    ("grep '^enabled' /etc/myos/services/hello.toml", "enabled = true"),
    # (output containing "# " would look like the prompt to this test)
    ("myctl cat netd | grep -c '^exec = '", "1"),
    # Phase 5: myctl, logs
    ("logger -t phase5 via-myctl; sleep 0.5; myctl logs phase5", "notice  phase5["),
    ("myctl logs -n 200 init | grep -q 'started sysinfo' && echo found", "found"),
    ("echo n=$(myctl logs -p err -g via-myctl | wc -l)", "n=0"),
    # Phase 5: myctl, settings
    ("myctl get net.eth0.method", "dhcp"),
    ("myctl set net.eth0.method bogus; echo rc=$?", "rc=1"),
    ("myctl set net.eth0.method static; echo rc=$?", "needs net.eth0.address"),
    ("myctl set net.eth0.method dhcp", "no changes"),
    ("myctl set hostname phase5 >/dev/null; hostname; grep 127.0.1.1 /etc/hosts", "127.0.1.1\tphase5"),
    ("myctl set hostname myos >/dev/null; hostname; cat /etc/hostname", "myos\nmyos"),
    ("myctl config | grep -E '^(hostname|net.eth0.method|service.netd.enabled) '", "service.netd.enabled"),
    ("myctl version", "myctl 0.5.0 (myos)"),
    ("python3 -c 'from myos.log.v1 import log_pb2; from myos.net.v1 import net_pb2; print(\"bindings ok\")'", "bindings ok"),
    # Phase 5: myos-install, refusals (the disk itself is installed below)
    ("myos-install --list", "/dev/vda"),
    ("myos-install --yes /dev/vda; echo rc=$?", "is in use (/"),
    ("myos-install /dev/vdb </dev/null; echo rc=$?", "pass --yes"),
    ("ls /boot/bzImage /usr/lib/myos/boot", "core.img"),
]

# Phase 6, for images with Java (not WITH_JAVA=0). The JVM starts slowly
# under QEMU without KVM, hence the waits.
APP = "app.org.myos.hello"
WAIT_APP = f"for i in $(seq 60); do myctl status {APP} | grep -q running && break; sleep 1; done; myctl status {APP}"
WAIT_APP_LOG = (f"for i in $(seq 90); do myos-log -t {APP} | grep -q 'hello from Java' && break; sleep 1; done; "
                f"myos-log -t {APP}")
APP_STATUS = ("python3 -c 'from myos.bus import Client; from google.protobuf.wrappers_pb2 import StringValue as S; "
              f"c = Client(); print(c.call(\"{APP}\", \"Status\", S(), S).value)'")
JAVA_CHECKS = [
    ("myos-bus-ping-java", 'pong: "hello from Java"'),
    ("java --list-modules | grep -c -E '^(java.base|java.logging|jdk.unsupported|java.net.http|jdk.crypto.ec)@'", "5"),
    ("readlink /usr/bin/java; ls /usr/lib/jvm 2>&1 | grep -c 'No such'", "../lib/myos/jdk/bin/java\n1"),
    ("jrun check /usr/share/myos/apps/org.myos.hello.mapp", "ok  org.myos.hello 1.0.0"),
    ("jrun install /usr/share/myos/apps/org.myos.hello.mapp", "installed org.myos.hello 1.0.0"),
    ("grep org.myos.hello /etc/passwd", "app_hello:x:20000:20000:myos app org.myos.hello:"),
    (WAIT_APP, "running   pid"),
    # The done-check: its own log entries (SDK, over the bus) and its stdout (through init) in logd.
    (WAIT_APP_LOG, "hello from Java: org.myos.hello 1.0.0 started (run 1)"),
    (f"myos-log -t {APP}", "stdout works too"),
    (f"myctl logs {APP} | grep -q 'heartbeat 1: load' && echo beat", "beat"),
    ("ps -o user= -C java", "app_hello"),
    ("grep -E '^(Uid|NoNewPrivs)' /proc/$(pidof java)/status", "Uid:\t20000\t20000\t20000\t20000\nNoNewPrivs:\t1"),
    ("readlink /proc/$(pidof java)/ns/net /proc/1/ns/net | uniq | wc -l", "2"),
    ("cat /etc/myos/bus/policy.d/20000", "call sysinfo"),
    ("cat /var/lib/myos/apps/org.myos.hello/data/runs", "1"),
    (APP_STATUS, "heartbeats"),
    ("jrun list | grep org.myos.hello", "app_hello"),
    (f"myctl restart {APP}", "running   pid"),
]

# Phase 5 done-check, part 1: install onto the empty second disk (/dev/vdb).
INSTALL = ("myos-install --yes --hostname installed /dev/vdb", "done: /dev/vdb boots myos (hostname installed)")

# Part 2: boot that disk alone, through GRUB, and look around.
INSTALLED_CHECKS = [
    ("cat /proc/cmdline", "root=PARTUUID="),
    ("hostname", "installed"),
    ("cat /sys/class/block/vda1/partition", "1"),
    ("grep ^root /etc/myos/installed", "root = PARTUUID="),
    ("id=$(grep -o 'PARTUUID=[0-9a-f]*' /proc/cmdline | cut -d= -f2); grep -c \"disk_id = $id\" /etc/myos/installed", "1"),
    ("echo up=$(myctl status | grep -c -E '^(busd|logd|netd|sysinfo|getty) +running ')", "up=5"),
    ("myctl status hello", "done      exit code 0"),
    ("myos-net status | tail -1", "online"),
    ("myctl logs -n 1000 kernel | grep -c 'Linux version'", "1"),
    # Fresh logs and history: nothing from the system it was installed from.
    ("grep -c -e phase3 -e phase5 /var/log/messages", "0"),
    # ([e] so this command, saved to history as it runs, doesn't match itself)
    ("grep -c 'msh-compl[e]te-test' /root/.msh_history", "0"),
]

# Phase 6 on the installed disk: the app came along and starts at boot.
INSTALLED_JAVA_CHECKS = [
    (WAIT_APP, "running   pid"),
    (WAIT_APP_LOG.replace("hello from Java", "started (run 3)"), "started (run 3)"),
    ("readlink /proc/$$/exe", "/usr/bin/msh"),
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


def login(con, timeout):
    t0 = time.monotonic()
    con.expect("login:", timeout)
    elapsed = time.monotonic() - t0
    con.send("root")
    con.expect("Password:", 10)
    con.send("myos")
    con.expect(PROMPT, 15)
    return elapsed


def run_checks(con, checks, failures, timeout=40):
    for cmd, want in checks:
        con.send(cmd)
        out = con.expect(PROMPT, timeout)
        # The console echoes the command line itself; only look at what follows it.
        out = out.split("\n", 1)[1] if "\n" in out else ""
        out = out.replace("\r\n", "\n")
        ok = want in out
        label = "wait for hello oneshot" if cmd is WAIT_HELLO else "reap orphans" if cmd is ZOMBIES else cmd
        print(f"{'PASS' if ok else 'FAIL'}  {label}")
        if not ok:
            failures.append(label)
            print(f"      expected {want!r} in:\n      " + out.strip().replace("\n", "\n      "))


def image_has_java(con):
    con.send("test -x /usr/lib/myos/jdk/bin/java && echo java-yes || echo java-no")
    return "java-yes" in con.expect(PROMPT, 20).split("\n", 1)[-1]


def power_off(con, proc, failures, label="poweroff"):
    # Phase 1 done-check: BusyBox poweroff signals our PID 1, which stops
    # the services, syncs, remounts / read-only and powers the VM off.
    con.send("poweroff")
    text = con.drain(30)
    try:
        proc.wait(timeout=10)  # QEMU closes its output a moment before it exits
    except subprocess.TimeoutExpired:
        pass
    for what, ok in [
        ("init stopped services", "[init] powering off: stopping services" in text),
        ("init synced filesystems", "[init] syncing filesystems" in text),
        ("kernel powered down", "reboot: Power down" in text),
        ("QEMU exited", proc.poll() is not None),
    ]:
        print(f"{'PASS' if ok else 'FAIL'}  {label}: {what}")
        if not ok:
            failures.append(f"{label}: {what}")


def boot(env, log_path, body):
    """Boots QEMU (scripts/run-qemu.sh with `env`) and runs body(con, proc, failures)."""
    proc = subprocess.Popen(
        [str(ROOT / "scripts" / "run-qemu.sh")],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        env=dict(os.environ, **env),
    )
    failures = []
    with open(log_path, "w") as log:
        con = Console(proc, log)
        try:
            body(con, proc, failures)
        except (TimeoutError, RuntimeError) as e:
            failures.append(str(e))
            print(f"FAIL  {e}")
        finally:
            if proc.poll() is None:
                proc.kill()
            proc.wait()
    return failures


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--timeout", type=int, default=180, help="seconds to wait for login prompt")
    ap.add_argument("--install-timeout", type=int, default=600, help="seconds myos-install may take")
    args = ap.parse_args()

    out_dir = pathlib.Path(os.environ.get("O", ROOT / "output"))
    out_dir.mkdir(exist_ok=True)
    log_path = out_dir / "boot_test.log"
    # Phase 5: an empty second disk to install onto, made fresh every run.
    disk = out_dir / "images" / "install-test.img"
    disk.parent.mkdir(exist_ok=True)
    with open(disk, "wb") as f:
        f.truncate(1 << 30)
    installed = {}

    def first_boot(con, proc, failures):
        print(f"PASS  reached login prompt in {login(con, args.timeout):.1f}s")
        run_checks(con, CHECKS, failures)
        has_java = image_has_java(con)
        installed["java"] = has_java
        if has_java:
            run_checks(con, JAVA_CHECKS, failures, timeout=120)
        else:
            print("SKIP  Phase 6 checks: no Java in this image (WITH_JAVA=0)")

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

        # Phase 5 done-check, part 1.
        t0 = time.monotonic()
        before = len(failures)
        run_checks(con, [INSTALL], failures, timeout=args.install_timeout)
        installed["ok"] = len(failures) == before
        if installed["ok"]:
            print(f"      (installed in {time.monotonic() - t0:.0f}s)")
        power_off(con, proc, failures)

    failures = boot({"EXTRA_DISK": str(disk)}, log_path, first_boot)

    # Phase 5 done-check, part 2: boot the installed disk on its own.
    if installed.get("ok"):
        def second_boot(con, proc, failures):
            con.expect("GNU GRUB", args.timeout)
            print("PASS  installed disk: GRUB menu")
            print(f"PASS  installed disk: reached login prompt in {login(con, args.timeout):.1f}s")
            run_checks(con, INSTALLED_CHECKS, failures)
            if installed.get("java"):
                run_checks(con, INSTALLED_JAVA_CHECKS, failures, timeout=120)
            power_off(con, proc, failures, label="installed disk: poweroff")

        log2 = out_dir / "boot_test_installed.log"
        failures += boot({"DISK": str(disk)}, log2, second_boot)
        print(f"\nconsole logs: {log_path}, {log2}")
    else:
        print(f"\nconsole log: {log_path}")
    if failures:
        print(f"Boot test FAILED ({len(failures)} failure(s))")
        return 1
    print("Boot test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
