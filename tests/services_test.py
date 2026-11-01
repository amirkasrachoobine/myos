#!/usr/bin/env python3
"""Host-side test of the Phase 3 services: no Buildroot build, no QEMU.

Builds busd (rust/bus) and the services (rust/services), then:

  logd + sysinfod  run against a private bus socket and log socket in a temp
                   directory, so nothing touches your own /dev/log; checked
                   through myos-log, myos-sysinfo and raw syslog datagrams.
  netd             runs in a network namespace whose only link is a veth
                   pair to a second namespace running dnsmasq as the DHCP
                   server; checked through myos-net. Needs root, `ip` and
                   dnsmasq (apt install dnsmasq-base); skipped otherwise.

Usage: python3 tests/services_test.py [--no-build]   (sudo for the netd part)
  --no-build   use the binaries already built (`make test-services` builds
               them as you, then runs this with sudo)
"""
import os
import pathlib
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUS_BIN = ROOT / "rust/bus/target/release"
SVC_BIN = ROOT / "rust/services/target/release"

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + ("" if ok or not detail else "\n      " + str(detail).strip().replace("\n", "\n      ")))


def skip(name, why):
    print(f"SKIP  {name}: {why}")


def wait_for(fn, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if fn():
            return True
        time.sleep(0.1)
    return fn()


def build():
    for d in ("rust/bus", "rust/services"):
        r = subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT / d)
        if r.returncode != 0:
            sys.exit(f"cargo build failed in {d}")


class Procs:
    def __init__(self):
        self.procs = {}

    def start(self, name, cmd, env, prefix=(), log_dir=None):
        out = open(log_dir / f"{name}.out", "w") if log_dir else subprocess.DEVNULL
        p = subprocess.Popen([*prefix, *cmd], env=env, stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
        self.procs[name] = p
        return p

    def stop(self, name):
        p = self.procs.pop(name, None)
        if p and p.poll() is None:
            os.killpg(p.pid, signal.SIGTERM)
            try:
                p.wait(3)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()

    def stop_all(self):
        for n in list(self.procs):
            self.stop(n)


def logd_and_sysinfo(tmp, procs):
    bus = tmp / "bus.sock"
    devlog = tmp / "log.sock"
    messages = tmp / "var/log/messages"
    env = dict(os.environ, MYOS_BUS_SOCKET=str(bus), MYOS_LOGD_SOCKET=str(devlog), MYOS_LOGD_FILE=str(messages),
               MYOS_LOGD_KMSG="none", MYOS_LOGD_STATE="none", MYOS_LOGD_MEMORY="200")

    def tool(*args, as_nobody=False):
        kw = {}
        if as_nobody:
            kw["preexec_fn"] = lambda: (os.setgid(65534), os.setuid(65534))
        r = subprocess.run([str(SVC_BIN / args[0]), *args[1:]], env=env, capture_output=True, text=True, timeout=20, **kw)
        return r.returncode, r.stdout + r.stderr

    procs.start("busd", [str(BUS_BIN / "busd"), "--socket", str(bus)], env, log_dir=tmp)
    check("busd up", wait_for(lambda: bus.exists()))
    procs.start("logd", [str(SVC_BIN / "logd")], env, log_dir=tmp)
    procs.start("sysinfod", [str(SVC_BIN / "sysinfod")], env, log_dir=tmp)
    check("logd listening on its syslog socket", wait_for(lambda: devlog.exists()))
    listed = lambda: subprocess.run([str(BUS_BIN / "myos-bus"), "list"], env=env, capture_output=True, text=True).stdout
    check("log and sysinfo registered on the bus",
          wait_for(lambda: re.search(r"^log ", listed(), re.M) and re.search(r"^sysinfo ", listed(), re.M), 8), listed())

    # --- syslog datagrams, as glibc/BusyBox send them
    s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    s.sendto(b"<11>Nov  1 12:00:00 tester[42]: something went wrong", str(devlog))
    s.sendto(b"<14>Nov  1 12:00:01 tester[42]: all good again", str(devlog))
    s.sendto(b"<14>" + b"x" * 6000, str(devlog))
    time.sleep(0.3)
    code, out = tool("myos-log", "-t", "tester")
    check("syslog datagrams are logged with tag and priority",
          "err     tester[" in out and "something went wrong" in out and "info    tester[" in out, out)
    check("the sender's pid comes from the kernel, not the message", f"tester[{os.getpid()}]" in out and "[42]" not in out, out)
    code, out = tool("myos-log", "-t", "tester", "-p", "warning")
    check("-p filters by severity", "went wrong" in out and "good again" not in out, out)
    code, out = tool("myos-log", "-g", "good")
    check("-g filters by text", "good again" in out and "went wrong" not in out, out)

    # --- bus writes
    code, out = tool("myos-log", "write", "-t", "bustest", "-p", "notice", "hello", "over", "the", "bus")
    check("myos-log write", code == 0 and "logged as entry" in out, out)
    code, out = tool("myos-log", "-t", "bustest", "-s")
    check("bus writes are logged, source shown with -s", "bus      notice  bustest[" in out and "hello over the bus" in out, out)
    code, out = tool("myos-log", "write", "-p", "loud", "x")
    check("bad priority rejected", code != 0 and "unknown priority" in out, out)

    # --- follow
    f = subprocess.Popen([str(SVC_BIN / "myos-log"), "-f", "-n", "0", "-t", "follow"], env=env, stdout=subprocess.PIPE, text=True)
    time.sleep(0.7)
    s.sendto(b"<14>follow: first", str(devlog))
    tool("myos-log", "write", "-t", "follow", "second")
    time.sleep(1.2)
    f.terminate()
    followed = f.communicate(timeout=5)[0]
    check("-f prints new entries as they arrive", "]: first" in followed and "second" in followed and followed.count("\n") == 2, followed)

    # --- non-root callers only see their own entries
    code, out = tool("myos-log", "write", "-t", "nobodys", "mine", as_nobody=True)
    check("a non-root user can write", code == 0, out)
    code, out = tool("myos-log", "-n", "1000", as_nobody=True)
    check("a non-root user only reads their own entries", "mine" in out and "tester" not in out, out)

    # --- stats and the file
    code, out = tool("myos-log", "stats")
    check("stats count the over-long datagram as malformed", "malformed  1" in out, out)
    text = messages.read_text() if messages.exists() else ""
    check("entries are written to the log file",
          re.search(r"^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{3} err     tester\[\d+\]: something went wrong$", text, re.M) is not None,
          text[-500:])

    # --- restart: queries work again, the file keeps growing
    before = len(text)
    procs.stop("logd")
    code, out = tool("myos-log")
    check("with logd stopped, myos-log says why", code != 0 and "UNKNOWN_SERVICE" in out, out)
    procs.start("logd", [str(SVC_BIN / "logd")], env, log_dir=tmp)
    check("restarted logd re-registers", wait_for(lambda: tool("myos-log", "write", "-t", "again", "back")[0] == 0, 8))
    check("restarted logd appends to the same file",
          wait_for(lambda: messages.read_text().find("again[") > before and messages.read_text().startswith(text[:50])))

    # --- sysinfo
    code, out = tool("myos-sysinfo")
    check("myos-sysinfo summary", code == 0 and "system  " in out and "memory  " in out and "cpu     " in out, out)
    check("hostname matches", socket.gethostname() in out, out)
    time.sleep(1.2)
    code, out = tool("myos-sysinfo", "cpu")
    m = re.search(r"sampled  over (\d+) ms", out)
    check("CPU usage is sampled about once a second", m is not None and 900 <= int(m.group(1)) <= 1600, out)
    code, out = tool("myos-sysinfo", "mem")
    total = re.search(r"total\s+([\d.]+) (\w+)", out)
    check("memory total is reported", total is not None and float(total.group(1)) > 0, out)
    code, out = tool("myos-sysinfo", "disks")
    check("the root filesystem is in disks", re.search(r"^/\s", out, re.M) is not None, out)
    code, out = tool("myos-sysinfo", "disks", "-a")
    check("disks -a includes proc", re.search(r"^/proc\s+proc", out, re.M) is not None, out)
    busy = subprocess.Popen([sys.executable, "-c", "while True: pass"])
    time.sleep(2.3)
    code, out = tool("myos-sysinfo", "ps")
    busy.kill()
    busy.wait()
    line = next((l for l in out.splitlines() if l.split()[:1] == [str(busy.pid)]), "")
    cpu = float(line.split()[4]) if line else -1
    check("ps lists processes with their CPU use", 50 <= cpu <= 101, line or out[:400])
    logd_pid = procs.procs["logd"].pid
    check("ps shows full command lines", re.search(rf"^\s*{logd_pid}\s.*{re.escape(str(SVC_BIN / 'logd'))}$", out, re.M) is not None)
    procs.stop("sysinfod")
    procs.stop("logd")
    procs.stop("busd")


def netd(tmp, procs):
    if os.geteuid() != 0:
        return skip("netd", "needs root for network namespaces (sudo python3 tests/services_test.py)")
    missing = [t for t in ("ip", "dnsmasq") if not shutil.which(t)]
    if missing:
        return skip("netd", f"needs {' and '.join(missing)} (apt install iproute2 dnsmasq-base)")

    tag = f"myos{os.getpid()}"
    cli, srv = f"{tag}c", f"{tag}s"
    sh = lambda *a: subprocess.run(a, capture_output=True, text=True)
    def in_cli(*a):
        return sh("ip", "netns", "exec", cli, *a)
    try:
        for ns in (cli, srv):
            sh("ip", "netns", "add", ns)
        sh("ip", "link", "add", "eth0", "netns", cli, "type", "veth", "peer", "name", "srv0", "netns", srv)
        sh("ip", "-n", srv, "addr", "add", "10.99.0.1/24", "dev", "srv0")
        sh("ip", "-n", srv, "link", "set", "srv0", "up")
        sh("ip", "-n", srv, "link", "set", "lo", "up")
        leases = tmp / "dnsmasq.leases"
        dns_log = tmp / "dnsmasq.out"
        procs.start("dnsmasq", ["ip", "netns", "exec", srv, "dnsmasq", "--keep-in-foreground", "--port=0", "--bind-interfaces",
                                "--interface=srv0", "--dhcp-range=10.99.0.50,10.99.0.60,12h", "--dhcp-option=option:router,10.99.0.1",
                                "--dhcp-option=option:dns-server,10.99.0.1", "--dhcp-option=option:domain-name,test.example",
                                f"--dhcp-leasefile={leases}", "--log-dhcp", "--log-facility=-", "--conf-file=/dev/null"], None, log_dir=tmp)
        time.sleep(0.5)
        check("dnsmasq (test DHCP server) running", procs.procs["dnsmasq"].poll() is None, (tmp / "dnsmasq.out").read_text())

        bus = tmp / "netbus.sock"
        conf = tmp / "netd.toml"
        resolv = tmp / "resolv.conf"
        conf.write_text('[eth0]\nmethod = "dhcp"\n\n[eth1]\nmethod = "static"\naddress = "192.168.77.5/24"\ndns = ["9.9.9.9"]\n')
        env = dict(os.environ, MYOS_BUS_SOCKET=str(bus), MYOS_NETD_CONFIG=str(conf), MYOS_NETD_RESOLV=str(resolv))
        prefix = ("ip", "netns", "exec", cli)
        procs.start("netbusd", [str(BUS_BIN / "busd"), "--socket", str(bus)], env, prefix, log_dir=tmp)
        wait_for(lambda: bus.exists())
        procs.start("netd", [str(SVC_BIN / "netd")], env, prefix, log_dir=tmp)

        def net(*args, as_nobody=False):
            kw = {"preexec_fn": (lambda: (os.setgid(65534), os.setuid(65534)))} if as_nobody else {}
            # The client needs no namespace: the bus socket is a path.
            r = subprocess.run([str(SVC_BIN / "myos-net"), *args], env=env, capture_output=True, text=True, timeout=30, **kw)
            return r.returncode, r.stdout + r.stderr

        t0 = time.monotonic()
        code, out = net("wait", "--timeout", "20")
        took = time.monotonic() - t0
        check(f"myos-net wait: online via DHCP in {took:.1f}s", code == 0 and re.search(r"eth0 10\.99\.0\.(5\d|60)/24", out) is not None,
              out + "\n" + (tmp / "netd.out").read_text())
        code, out = net("status")
        check("lo is 127.0.0.1/8", re.search(r"^lo\s+up\s+loopback\s+127\.0\.0\.1/8\s+configured", out, re.M) is not None, out)
        check("eth0 bound with gateway, dns and lease", re.search(r"^eth0\s+up\s+dhcp\s+10\.99\.0\.\d+/24\s+bound", out, re.M) is not None
              and "gateway 10.99.0.1" in out and "dns 10.99.0.1" in out and "lease from 10.99.0.1" in out, out)
        route = in_cli("cat", "/proc/net/route").stdout
        check("default route via 10.99.0.1", re.search(r"^eth0\s+00000000\s+0100630A", route, re.M) is not None, route)
        check("resolv.conf written", "search test.example\nnameserver 10.99.0.1\n" in (resolv.read_text() if resolv.exists() else ""),
              resolv.read_text() if resolv.exists() else "missing")
        if shutil.which("ping"):
            ping = in_cli("ping", "-c", "1", "-W", "2", "10.99.0.1")
            check("the DHCP server is reachable over the new address", ping.returncode == 0, ping.stdout + ping.stderr)

        # Hotplug: eth1 appears after netd started; it's in netd.toml as static.
        sh("ip", "link", "add", "eth1", "netns", cli, "type", "veth", "peer", "name", "srv1", "netns", srv)
        sh("ip", "-n", srv, "link", "set", "srv1", "up")
        check("an interface that appears later is configured (static)",
              wait_for(lambda: re.search(r"^eth1\s+\S+\s+static\s+192\.168\.77\.5/24\s+configured", net("status")[1], re.M) is not None, 6),
              net("status")[1])
        check("resolv.conf has both interfaces' servers",
              wait_for(lambda: "nameserver 10.99.0.1\nnameserver 9.9.9.9\n" in resolv.read_text()), resolv.read_text())

        # Renew: root only; a unicast REQUEST that the server ACKs.
        code, out = net("renew", "eth0", as_nobody=True)
        check("renew is refused for non-root", code != 0 and "only root" in out, out)
        acks = lambda: (tmp / "dnsmasq.out").read_text().count("DHCPACK(srv0)")
        before = acks()
        code, out = net("renew", "eth0")
        check("renew as root", code == 0, out)
        check("the server ACKs the renewal", wait_for(lambda: acks() > before, 8), (tmp / "dnsmasq.out").read_text()[-800:])
        check("renewal leaves the interface bound",
              wait_for(lambda: re.search(r"^eth0\s+up\s+dhcp\s+10\.99\.0\.\d+/24\s+bound", net("status")[1], re.M) is not None),
              net("status")[1])
        code, out = net("renew", "eth1")
        check("renew on a static interface is an error", code != 0 and "not dhcp" in out, out)

        # Restart: gets the same address back without disturbing it.
        addr = re.search(r"eth0\s+up\s+dhcp\s+(10\.99\.0\.\d+)", net("status")[1]).group(1)
        procs.stop("netd")
        procs.start("netd", [str(SVC_BIN / "netd")], env, prefix, log_dir=tmp)
        code, out = net("wait", "--timeout", "20")
        check("after a netd restart: same address again", code == 0 and f"eth0 {addr}/24" in out, out)

        # Someone downs the interface: the kernel drops the default route with it.
        in_cli("ip", "link", "set", "eth0", "down")
        has_route = lambda: re.search(r"^eth0\s+00000000\s+0100630A", in_cli("cat", "/proc/net/route").stdout, re.M) is not None
        check("netd brings a downed interface back up, with its default route",
              wait_for(lambda: has_route() and re.search(r"^eth0\s+up\s+dhcp\s+10\.99\.0\.\d+/24\s+bound", net("status")[1], re.M), 15),
              net("status")[1] + in_cli("cat", "/proc/net/route").stdout)
        out = (tmp / "netd.out").read_text()
        check("netd logs with syslog priority prefixes", re.search(r"^<6>eth0: 10\.99\.0\.\d+/24 from 10\.99\.0\.1", out, re.M) is not None, out)
    finally:
        procs.stop("netd")
        procs.stop("netbusd")
        procs.stop("dnsmasq")
        for ns in (cli, srv):
            sh("ip", "netns", "del", ns)


def main():
    if "--no-build" not in sys.argv[1:]:
        build()
    for b in (BUS_BIN / "busd", SVC_BIN / "logd"):
        if not b.exists():
            sys.exit(f"{b} not found; build first (make test-services does)")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="myos-services-test-"))
    os.chmod(tmp, 0o755)
    procs = Procs()
    try:
        logd_and_sysinfo(tmp, procs)
        netd(tmp, procs)
    finally:
        procs.stop_all()
    if all(results):
        print(f"all {len(results)} checks passed")
        shutil.rmtree(tmp, ignore_errors=True)
        return 0
    print(f"{results.count(False)} of {len(results)} checks FAILED; logs in {tmp}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
