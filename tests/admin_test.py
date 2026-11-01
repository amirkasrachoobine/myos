#!/usr/bin/env python3
"""Host-side test of the Phase 5 admin tools: no Buildroot build.

  tomledit, settings   edits keep comments and parse (with init's own parser
                       too, via `myos-init check`); myctl get/set/unset in a
                       scratch root ($MYOS_SYSROOT)
  myctl logs           against busd + logd on private sockets
  myctl services       against the real myos-init as PID 1 in namespaces,
                       as tests/init_ns_test.py runs it (root only)
  disk, copy           MBR layout (checked with sfdisk when present); copying
                       a tree keeps links, devices, owners, modes and times
  myos-install         onto an image file through a loop device, with GRUB
                       images made by this host's grub-mkimage; then, when
                       QEMU and a kernel are available, the image is booted
                       through GRUB and must reach its init (root only)

Usage: python3 tests/admin_test.py [--no-build]   (sudo for the root parts)
  MYOS_TEST_KERNEL=/path/to/bzImage picks the kernel for the boot check
  (default: the newest readable /boot/vmlinuz-*).
"""
import glob
import os
import re
import pathlib
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path[:0] = [str(ROOT / "python/admin"), str(ROOT / "python/bus")]
PYPATH = f"{ROOT / 'python/admin'}:{ROOT / 'python/bus'}"
INIT = ROOT / "rust/init/target/release/myos-init"
BUS_BIN = ROOT / "rust/bus/target/release"
SVC_BIN = ROOT / "rust/services/target/release"
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
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


def myctl(*args, env=None, prefix=()):
    e = dict(os.environ, PYTHONPATH=PYPATH, **(env or {}))
    r = subprocess.run([*prefix, sys.executable, str(ROOT / "python/admin/myctl"), *args],
                       env=e, capture_output=True, text=True, timeout=60)
    return r.returncode, r.stdout + r.stderr


def build():
    subprocess.run(["make", "-s", "-C", ROOT / "python/bus"], check=True)
    for d in ("rust/init", "rust/bus", "rust/services"):
        r = subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT / d)
        if r.returncode != 0:
            sys.exit(f"cargo build failed in {d}")


# ---- tomledit and settings ---------------------------------------------------

def test_tomledit(tmp):
    from myos.admin import tomledit as te

    src = """# a comment that must survive
description = "x"   # inline
exec = ["/bin/sleep",
        "1000"]

[env]
# env comment
A = "1"
"""
    t = te.set_value(src, None, "enabled", False)
    check("tomledit: top-level key added after the last one, comments kept",
          t.startswith("# a comment that must survive\n") and '        "1000"]\nenabled = false\n\n[env]' in t, t)
    t2 = te.set_value(t, None, "enabled", True)
    check("tomledit: existing key replaced in place", t2.count("enabled") == 1 and "enabled = true" in t2, t2)
    t3 = te.set_value(src, None, "exec", ["/bin/true"])
    check("tomledit: multi-line value replaced whole", 'exec = ["/bin/true"]\n\n[env]' in t3 and '"1000"' not in t3, t3)
    t4 = te.set_value(src, "env", "B", 'quote " back\\slash')
    check("tomledit: key added to a table, string escaped", 'A = "1"\nB = "quote \\" back\\\\slash"' in t4, t4)
    t5 = te.set_value(src, "new", "k", 3)
    check("tomledit: new table appended", t5.endswith('A = "1"\n\n[new]\nk = 3\n'), t5)
    t6 = te.delete_table(t5, "env")
    check("tomledit: table removed with its keys", "[env]" not in t6 and "A =" not in t6 and "[new]" in t6, t6)
    check("tomledit: key removed", "description" not in te.delete_key(src, None, "description"))
    try:
        te.set_value(src, None, "a.b", 1)
        check("tomledit: dotted keys refused", False)
    except te.EditError:
        check("tomledit: dotted keys refused", True)

    # init's own parser must accept what we write.
    svc = tmp / "svc"
    svc.mkdir()
    (svc / "edited.toml").write_text(te.set_value(src, None, "enabled", False))
    r = subprocess.run([str(INIT), "check", str(svc)], capture_output=True, text=True)
    check("myos-init check accepts an edited service file", r.returncode == 0 and "[disabled]" in r.stdout, r.stdout + r.stderr)


def make_sysroot(tmp):
    r = tmp / "root"
    shutil.copytree(ROOT / "build/board/qemu-x86_64/rootfs-overlay/etc/myos", r / "etc/myos")
    (r / "etc/hostname").write_text("myos\n")
    (r / "etc/hosts").write_text("127.0.0.1\tlocalhost\n127.0.1.1\tmyos\n")
    return r


def test_settings(tmp):
    r = make_sysroot(tmp)
    env = {"MYOS_SYSROOT": str(r)}
    code, out = myctl("config", env=env)
    check("config lists hostname, net and services",
          code == 0 and "hostname" in out and "net.eth0.method          dhcp" in out and "service.getty.enabled" in out, out)
    code, out = myctl("set", "--no-apply", "hostname", "box", "net.eth1.method", "static",
                      "net.eth1.address", "192.168.7.2/24", "net.eth1.gateway", "192.168.7.1",
                      "net.eth1.dns", "1.1.1.1, 9.9.9.9", "service.hello.enabled", "no", env=env)
    check("set several keys at once", code == 0 and "wrote /etc/myos/netd.toml" in out, out)
    netd = (r / "etc/myos/netd.toml").read_text()
    check("netd.toml: new table, comments kept",
          netd.startswith("# Interfaces netd configures") and
          '[eth1]\nmethod = "static"\naddress = "192.168.7.2/24"\ngateway = "192.168.7.1"\ndns = ["1.1.1.1", "9.9.9.9"]\n' in netd, netd)
    check("hostname and the 127.0.1.1 line of /etc/hosts",
          (r / "etc/hostname").read_text() == "box\n" and "127.0.1.1\tbox" in (r / "etc/hosts").read_text()
          and "127.0.1.1\tmyos" not in (r / "etc/hosts").read_text())
    rc = subprocess.run([str(INIT), "check", str(r / "etc/myos/services")], capture_output=True, text=True)
    check("service.hello.enabled false, as init reads it", "hello" in rc.stdout and "[disabled]" in rc.stdout.split("hello", 1)[1].split("\n")[0], rc.stdout)
    code, out = myctl("get", "net.eth1.dns", env=env)
    check("get a list", out.strip() == "1.1.1.1,9.9.9.9", out)
    code, out = myctl("set", "net.eth2.method", "static", env=env)
    check("static without an address is refused, nothing written",
          code == 1 and "needs net.eth2.address" in out and "eth2" not in (r / "etc/myos/netd.toml").read_text(), out)
    for args, why in [(("set", "hostname", "bad_name"), "valid hostname"), (("set", "net.eth0.address", "10.0.0.1"), "prefix length"),
                      (("set", "net.eth0.gateway", "x"), "not an IPv4"), (("set", "service.nosuch.enabled", "true"), "no service named"),
                      (("set", "frob", "1"), "unknown setting"), (("set", "hostname"), "KEY VALUE pairs"),
                      (("unset", "net.eth0.method"), "unset the whole interface")]:
        code, out = myctl(*args, env=env)
        check(f"refused: myctl {' '.join(args)}", code in (1, 2) and why in out, out)
    code, out = myctl("set", "--no-apply", "net.eth1.method", "dhcp", env=env)
    netd = (r / "etc/myos/netd.toml").read_text()
    check("switching to dhcp drops the static-only keys",
          code == 0 and "removed net.eth1.address" in out and "192.168.7" not in netd and 'method = "dhcp"' in netd.split("[eth1]")[1], out + netd)
    code, out = myctl("unset", "--no-apply", "net.eth1", env=env)
    check("unset an interface", code == 0 and "\n[eth1]" not in (r / "etc/myos/netd.toml").read_text(), out)
    code, out = myctl("set", "--no-apply", "net.eth0.method", "dhcp", env=env)
    check("no-op set says so", code == 0 and "no changes" in out, out)
    (r / "etc/myos/netd.toml").unlink()
    code, out = myctl("get", "net.eth0.method", env=env)
    check("without netd.toml, the default is reported", "dhcp (default" in out, out)
    code, out = myctl("set", "--no-apply", "net.eth0.method", "dhcp", env=env)
    check("creating netd.toml warns that only listed interfaces are managed",
          code == 0 and "only manages the interfaces listed" in out and (r / "etc/myos/netd.toml").exists(), out)


# ---- myctl logs ----------------------------------------------------------------

class Procs:
    def __init__(self):
        self.procs = {}

    def start(self, name, cmd, env, log_dir):
        p = subprocess.Popen(cmd, env=env, stdout=open(log_dir / f"{name}.out", "w"), stderr=subprocess.STDOUT, start_new_session=True)
        self.procs[name] = p
        return p

    def stop_all(self):
        for p in self.procs.values():
            if p.poll() is None:
                os.killpg(p.pid, signal.SIGTERM)
                p.wait(5)


def test_logs(tmp):
    bus, devlog = tmp / "bus.sock", tmp / "log.sock"
    env = dict(os.environ, MYOS_BUS_SOCKET=str(bus), MYOS_LOGD_SOCKET=str(devlog), MYOS_LOGD_FILE=str(tmp / "messages"),
               MYOS_LOGD_KMSG="none", MYOS_LOGD_STATE="none")
    procs = Procs()
    try:
        procs.start("busd", [str(BUS_BIN / "busd"), "--socket", str(bus)], env, tmp)
        wait_for(bus.exists)
        procs.start("logd", [str(SVC_BIN / "logd")], env, tmp)
        wait_for(devlog.exists)
        benv = {"MYOS_BUS_SOCKET": str(bus)}
        s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        for i, (pri, msg) in enumerate([(30, "first info"), (27, "an error"), (30, "second info")]):
            s.sendto(f"<{pri}>tester: {msg}".encode(), str(devlog))
        wait_for(lambda: "second info" in myctl("logs", "tester", env=benv)[1])
        code, out = myctl("logs", "tester", env=benv)
        lines = out.strip().splitlines()
        check("logs SERVICE, oldest first, myos-log's format",
              code == 0 and len(lines) == 3 and re.search(r"^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{3} info    tester\[\d+\]: first info$", lines[0])
              and re.search(r" err     tester\[\d+\]: an error$", lines[1]), out)
        check("-p err", myctl("logs", "-p", "err", "tester", env=benv)[1].strip().endswith("an error"))
        check("-g TEXT", myctl("logs", "-g", "second", env=benv)[1].strip().endswith("second info"))
        check("-n 1", len(myctl("logs", "-n", "1", "tester", env=benv)[1].strip().splitlines()) == 1)
        code, out = myctl("logs", "-p", "loud", env=benv)
        check("unknown priority is a usage error", code == 2 and "unknown priority" in out, out)
        f = subprocess.Popen([sys.executable, str(ROOT / "python/admin/myctl"), "logs", "-f", "-n", "0", "follow"],
                             env=dict(os.environ, PYTHONPATH=PYPATH, **benv), stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        try:
            time.sleep(1.5)
            s.sendto(b"<30>follow: live line", str(devlog))
            got = []
            os.set_blocking(f.stdout.fileno(), False)

            def read():
                try:
                    got.append(os.read(f.stdout.fileno(), 65536).decode())
                except BlockingIOError:
                    pass
                return "live line" in "".join(got)
            wait_for(read, 6)
        finally:
            f.kill()
            f.wait()
        check("logs -f shows new entries", "live line" in "".join(got), got)
        logd = procs.procs.pop("logd")
        os.killpg(logd.pid, signal.SIGTERM)
        logd.wait(5)
        code, out = myctl("logs", env=benv)
        check("without logd: a clear error", code == 1 and "logd isn't on the bus" in out, out)
    finally:
        procs.stop_all()
    code, out = myctl("logs", env={"MYOS_BUS_SOCKET": str(tmp / "nobody.sock")})
    check("without busd: a clear error", code == 1 and "is busd running?" in out, out)


# ---- myctl services against the real init --------------------------------------

def test_services(tmp):
    if os.geteuid() != 0 or not shutil.which("unshare") or not shutil.which("nsenter"):
        skip("myctl services against myos-init", "needs root, unshare and nsenter")
        return
    work = pathlib.Path(tempfile.mkdtemp(prefix="myos-admin-ns-", dir=ROOT / "rust/init/target"))
    svc = work / "services"
    svc.mkdir()
    (svc / "sleeper.toml").write_text('description = "a long sleep"\nexec = ["/bin/sleep", "1000"]\nrestart = "always"\n')
    (svc / "once.toml").write_text('description = "a oneshot"\ntype = "oneshot"\nexec = ["/bin/sh", "-c", "sleep 0.5"]\n')
    (svc / "later.toml").write_text('exec = ["/bin/sleep", "1000"]\nenabled = false\n')
    console = work / "console.log"
    console.touch()
    setup = (f"mkdir -p /etc/myos/services && mount --bind {svc} /etc/myos/services && "
             f"mount --bind {console} /dev/console && exec {INIT}")
    env = dict(os.environ, MYOS_INIT_NO_ROOT_REMOUNT="1", MYOS_INIT_LOG_SOCKET="none")
    proc = subprocess.Popen(["unshare", "--fork", "--pid", "--mount", "--uts", "--propagation", "private", "--mount-proc",
                             "sh", "-c", setup], env=env)
    try:
        pid = None
        for _ in range(50):
            try:
                kids = pathlib.Path(f"/proc/{proc.pid}/task/{proc.pid}/children").read_text().split()
                if kids and os.readlink(f"/proc/{kids[0]}/exe") == str(INIT):
                    pid = int(kids[0])
                    break
            except OSError:
                pass
            time.sleep(0.1)
        if pid is None:
            check("init started in its namespace", False)
            return
        ns = ("nsenter", "-t", str(pid), "-m", "-p", "-u", "--")

        def ctl(*a):
            return myctl(*a, prefix=ns)

        wait_for(lambda: "running" in ctl("status", "sleeper")[1])
        code, out = ctl("status")
        check("status: a table with descriptions", code == 0 and out.startswith("SERVICE") and "a long sleep" in out
              and "later" in out and "disabled" in out, out)
        code, out = ctl("status", "later")
        check("status of a stopped service exits 3", code == 3, out)
        code, out = ctl("stop", "sleeper")
        check("stop", code == 0 and "stopped" in out, out)
        code, out = ctl("start", "sleeper")
        check("start", code == 0 and "running   pid" in out, out)
        old = out.split("pid ")[1].split(",")[0]
        code, out = ctl("restart", "sleeper")
        check("restart: a new pid", code == 0 and "running   pid" in out and out.split("pid ")[1].split(",")[0] != old, out)
        code, out = ctl("restart", "once")
        check("restart of a oneshot waits for it to finish", code == 0 and "done      exit code 0" in out, out)
        code, out = ctl("start", "nosuch")
        check("unknown service", code == 1 and "no service named nosuch" in out, out)
        code, out = ctl("enable", "later")
        check("enable: file edited, init reloaded, service started",
              code == 0 and "running" in out and "enabled = true" in (svc / "later.toml").read_text(), out)
        code, out = ctl("disable", "sleeper")
        check("disable: file edited and service stopped",
              code == 0 and "disabled" in out and "enabled = false" in (svc / "sleeper.toml").read_text(), out)
        check("disabled file still parses for init", subprocess.run([str(INIT), "check", str(svc)], capture_output=True).returncode == 0)
        code, out = ctl("cat", "once")
        check("cat", out.startswith("# /etc/myos/services/once.toml\n") and "oneshot" in out, out)
        code, out = ctl("reload")
        check("reload", code == 0 and "no changes" in out, out)
    finally:
        try:
            myctl("poweroff", prefix=("nsenter", "-t", str(pid), "-m", "-p", "-u", "--")) if pid else None
            proc.wait(15)
        except (subprocess.TimeoutExpired, NameError):
            proc.kill()
        shutil.rmtree(work, ignore_errors=True)
    code, out = myctl("status", env={"MYOS_INIT_SOCKET": str(tmp / "nope.sock")})
    check("without init: a clear error", code == 1 and "can't reach init" in out, out)


# ---- disk and copy -------------------------------------------------------------

def test_disk(tmp):
    from myos.admin import disk

    boot = bytes(range(256)) * 2
    m = disk.mbr([(2048, 409600, 0x83, True)], 0x1234ABCD, boot)
    check("MBR: 512 bytes, 55AA, boot code up to 440 bytes",
          len(m) == 512 and m[510:] == b"\x55\xaa" and m[:440] == boot[:440] and m[444:446] == b"\0\0")
    sig, parts = disk.read_mbr(m)
    check("MBR: reads back", sig == 0x1234ABCD and parts == [(2048, 409600, 0x83, True)], (hex(sig), parts))
    check("MBR: CHS of sector 2048 is 0/32/33", m[447:450] == bytes([32, 33, 0]), m[446:462].hex())
    check("parse_size", disk.parse_size("2G") == 2 << 30 and disk.parse_size("512MiB") == 512 << 20 and disk.parse_size("1.5k") == 1536)
    if shutil.which("sfdisk"):
        img = tmp / "mbr.img"
        with open(img, "wb") as f:
            f.truncate(256 << 20)
            f.write(disk.mbr([(2048, (256 << 20) // 512 - 2048, 0x83, True)], 0xCAFEF00D))
        out = subprocess.run(["sfdisk", "-d", str(img)], capture_output=True, text=True).stdout
        check("sfdisk reads our MBR", "label-id: 0xcafef00d" in out and "start=        2048" in out and "type=83, bootable" in out, out)
    else:
        skip("sfdisk reads our MBR", "no sfdisk")


def test_copy(tmp):
    from myos.admin.copy import Copier

    src, dst = tmp / "src", tmp / "dst"
    (src / "etc").mkdir(parents=True)
    (src / "etc/conf").write_text("conf\n")
    os.chmod(src / "etc/conf", 0o640)
    (src / "bin").mkdir()
    (src / "bin/tool").write_bytes(b"\x7fELF" + b"x" * 100000)
    os.chmod(src / "bin/tool", 0o4755 if os.geteuid() == 0 else 0o755)
    os.link(src / "bin/tool", src / "bin/tool2")
    os.symlink("tool", src / "bin/link")
    os.symlink("/nonexistent", src / "bin/dangling")
    os.mkfifo(src / "fifo")
    (src / "tmp").mkdir()
    (src / "tmp/junk").write_text("junk")
    (src / "root").mkdir()
    (src / "root/.msh_history").write_text("secret")
    (src / "root/keep").write_text("keep")
    os.utime(src / "etc/conf", ns=(1_000_000_000_000_000_000, 1_500_000_000_123_456_789))
    os.utime(src / "etc", ns=(1_000_000_000_000_000_000, 1_400_000_000_000_000_000))
    try:
        os.setxattr(src / "etc/conf", "user.myos", b"yes")
        xattr = True
    except OSError:
        xattr = False
    if os.geteuid() == 0:
        os.mknod(src / "null", 0o666 | stat.S_IFCHR, os.makedev(1, 3))
        os.chown(src / "etc/conf", 1000, 1001)
    dst.mkdir()
    reports = []
    c = Copier(empty=["tmp"], skip=["root/.msh_history"], progress=lambda d, t: reports.append((d, t)))
    c.copy(src, dst)
    check("regular files, contents and modes", (dst / "etc/conf").read_text() == "conf\n"
          and stat.S_IMODE(os.stat(dst / "etc/conf").st_mode) == 0o640)
    check("setuid kept" if os.geteuid() == 0 else "exec bit kept",
          stat.S_IMODE(os.stat(dst / "bin/tool").st_mode) == (0o4755 if os.geteuid() == 0 else 0o755))
    check("hard links stay hard links", os.stat(dst / "bin/tool").st_ino == os.stat(dst / "bin/tool2").st_ino)
    check("symlinks, dangling ones too", os.readlink(dst / "bin/link") == "tool" and os.readlink(dst / "bin/dangling") == "/nonexistent")
    check("FIFOs", stat.S_ISFIFO(os.lstat(dst / "fifo").st_mode))
    check("file and directory mtimes (ns)", os.stat(dst / "etc/conf").st_mtime_ns == 1_500_000_000_123_456_789
          and os.stat(dst / "etc").st_mtime_ns == 1_400_000_000_000_000_000)
    check("empty: created, not filled", (dst / "tmp").is_dir() and not any((dst / "tmp").iterdir()))
    check("skip: left out", not (dst / "root/.msh_history").exists() and (dst / "root/keep").exists())
    check("progress reported up to the total", reports and reports[-1][0] == reports[-1][1] == c.measure(src), reports[-3:])
    if xattr:
        check("extended attributes", os.getxattr(dst / "etc/conf", "user.myos") == b"yes")
    if os.geteuid() == 0:
        check("device nodes", os.stat(dst / "null").st_rdev == os.makedev(1, 3))
        check("owners", (os.stat(dst / "etc/conf").st_uid, os.stat(dst / "etc/conf").st_gid) == (1000, 1001))


# ---- myos-install ----------------------------------------------------------------

def find_kernel():
    k = os.environ.get("MYOS_TEST_KERNEL")
    if k:
        return k if os.access(k, os.R_OK) else None
    for k in sorted(glob.glob("/boot/vmlinuz-*"), reverse=True):
        if os.access(k, os.R_OK):
            return k
    return None


def test_install(tmp):
    if os.geteuid() != 0 or not os.path.exists("/dev/loop-control"):
        skip("myos-install onto an image file", "needs root and loop devices")
        return
    if not shutil.which("grub-mkimage") or not os.path.exists("/usr/lib/grub/i386-pc/boot.img"):
        skip("myos-install onto an image file", "needs GRUB for BIOS (apt install grub-pc-bin)")
        return
    busybox = shutil.which("busybox")
    if not busybox or "statically" not in subprocess.run(["file", "-L", busybox], capture_output=True, text=True).stdout:
        skip("myos-install onto an image file", "needs a static busybox (apt install busybox-static)")
        return
    grub = tmp / "grub"
    grub.mkdir()
    subprocess.run(["grub-mkimage", "-d", "/usr/lib/grub/i386-pc", "-O", "i386-pc", "-o", str(grub / "core.img"),
                    "-p", "(hd0,msdos1)/boot/grub", "boot", "linux", "ext2", "fat", "part_msdos", "part_gpt",
                    "normal", "biosdisk", "serial", "terminal"], check=True)
    shutil.copy("/usr/lib/grub/i386-pc/boot.img", grub / "boot.img")
    src = tmp / "system"
    for d in ("bin", "sbin", "etc", "proc", "sys", "dev", "run", "tmp", "boot", "var/log", "root"):
        (src / d).mkdir(parents=True)
    shutil.copy(busybox, src / "bin/busybox")
    for a in ("sh", "mount", "cat", "poweroff", "sync"):
        os.symlink("busybox", src / "bin" / a)
    (src / "sbin/init").write_text("#!/bin/sh\nmount -t proc proc /proc\n"
                                   'echo "MARKER cmdline=$(cat /proc/cmdline) hostname=$(cat /etc/hostname)"\n'
                                   "sync\npoweroff -f\n")
    os.chmod(src / "sbin/init", 0o755)
    (src / "etc/hostname").write_text("source\n")
    (src / "var/log/messages").write_text("old log\n")
    (src / "root/.msh_history").write_text("old history\n")
    kernel = find_kernel()
    (src / "boot/bzImage").write_bytes(open(kernel, "rb").read() if kernel else b"not a kernel")

    img = tmp / "disk.img"
    r = subprocess.run([sys.executable, str(ROOT / "python/admin/myos-install"), "--image-size", "128M", "--source", str(src),
                        "--grub-dir", str(grub), "--hostname", "target", "--cmdline", "console=ttyS0", str(img)],
                       env=dict(os.environ, PYTHONPATH=PYPATH), capture_output=True, text=True, timeout=300)
    out = r.stdout + r.stderr
    check("myos-install onto an image file", r.returncode == 0 and "[4/4]" in out and "done:" in out, out)
    if r.returncode != 0:
        return
    from myos.admin import disk
    data = open(img, "rb").read(1 << 20)
    sig, parts = disk.read_mbr(data[:512])
    boot_img = (grub / "boot.img").read_bytes()
    core = (grub / "core.img").read_bytes()
    check("MBR: one bootable Linux partition from 1 MiB to the end", parts == [(2048, (128 << 20) // 512 - 2048, 0x83, True)], parts)
    check("GRUB boot code in the MBR, core image after it", data[:440] == boot_img[:440] and data[512:512 + len(core)] == core)
    check("disk id printed as the PARTUUID", f"root=PARTUUID={sig:08x}-01" in out, out)

    def debugfs(cmd):
        return subprocess.run(["debugfs", "-R", cmd, str(part)], capture_output=True, text=True).stdout

    # Look inside the filesystem without mounting it: extract the partition.
    part = tmp / "part.img"
    with open(img, "rb") as fi, open(part, "wb") as fo:
        fi.seek(2048 * 512)
        shutil.copyfileobj(fi, fo)
    if shutil.which("debugfs"):
        cfg = debugfs("cat /boot/grub/grub.cfg")
        check("grub.cfg boots the disk by PARTUUID", f"linux /boot/bzImage root=PARTUUID={sig:08x}-01 rootwait console=ttyS0" in cfg, cfg)
        check("hostname set on the target", debugfs("cat /etc/hostname").strip() == "target")
        check("/var/log came over empty, history left out",
              "messages" not in debugfs("ls /var/log") and ".msh_history" not in debugfs("ls -l /root"))
        check("/etc/myos/installed records the disk", f"disk_id = {sig:08x}" in debugfs("cat /etc/myos/installed"))
        label = subprocess.run(["dumpe2fs", "-h", str(part)], capture_output=True, text=True).stdout
        check("ext4 labelled myos-root", "Filesystem volume name:   myos-root" in label and "extent" in label)
    else:
        skip("looking inside the installed filesystem", "no debugfs (e2fsprogs)")

    r = subprocess.run([sys.executable, str(ROOT / "python/admin/myos-install"), "--source", str(src), "--grub-dir", str(grub), str(img)],
                       env=dict(os.environ, PYTHONPATH=PYPATH), capture_output=True, text=True, stdin=subprocess.DEVNULL)
    check("an existing disk without --yes and no terminal: refused", r.returncode == 1 and "pass --yes" in r.stderr, r.stderr)
    small = tmp / "small.img"
    r = subprocess.run([sys.executable, str(ROOT / "python/admin/myos-install"), "--image-size", "8M", "--source", str(src),
                        "--grub-dir", str(grub), str(small)], env=dict(os.environ, PYTHONPATH=PYPATH), capture_output=True, text=True)
    check("a disk too small: refused before writing", r.returncode == 1 and "too small" in r.stderr, r.stderr)

    qemu = shutil.which("qemu-system-x86_64")
    if not qemu or not kernel:
        skip("booting the installed image", "needs qemu-system-x86_64 and a kernel (MYOS_TEST_KERNEL)")
        return
    accel = ["-accel", "kvm", "-cpu", "host"] if os.access("/dev/kvm", os.W_OK) else ["-accel", "tcg"]
    r = subprocess.run([qemu, "-M", "pc", *accel, "-m", "512", "-drive", f"file={img},if=virtio,format=raw",
                        "-nographic", "-no-reboot"], capture_output=True, text=True, timeout=300, stdin=subprocess.DEVNULL)
    boot = r.stdout
    check("BIOS → GRUB → kernel → the installed init",
          "GNU GRUB" in boot and f"MARKER cmdline=BOOT_IMAGE=/boot/bzImage root=PARTUUID={sig:08x}-01" in boot and "hostname=target" in boot,
          boot[-1500:])


def main():
    if "--no-build" not in sys.argv[1:]:
        build()
    for b in (INIT, BUS_BIN / "busd", SVC_BIN / "logd"):
        if not b.exists():
            sys.exit(f"{b} not found; build first (make test-admin does)")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="myos-admin-test-"))
    os.chmod(tmp, 0o755)
    for t in (test_tomledit, test_settings, test_logs, test_services, test_disk, test_copy, test_install):
        d = tmp / t.__name__
        d.mkdir()
        t(d)
    if all(results):
        print(f"all {len(results)} checks passed")
        shutil.rmtree(tmp, ignore_errors=True)
        return 0
    print(f"{results.count(False)} of {len(results)} checks FAILED; files in {tmp}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
