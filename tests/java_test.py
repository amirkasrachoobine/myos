#!/usr/bin/env python3
"""Phase 6 host test: the Java app runtime, without Buildroot or QEMU.

Part 1 (as anyone): builds the bus jar, the SDK, the sample app and the
trimmed JDK (jlink), and checks jrun's package handling: good and bad
packages, `jrun pack`, the manifest rules.

Part 2 (root): runs myos-init as PID 1 in fresh PID, mount and UTS
namespaces, with busd, logd and sysinfod as its services, the way the image
does. Then, inside:
  - jrun install of the sample app: its user, data directory, bus policy
    and service file, and init starting it as app.org.myos.hello
  - the done-check: the app runs as a service, under its own uid, with no
    network and no_new_privs, and its log entries reach logd (through the
    SDK over the bus, and its stdout through init)
  - its bus service answers; myctl sees it
  - the probe app (tests/java/probe) tries what it may not do and
    reports what the system let through
  - upgrade (uid and data kept), stop (graceful), remove (all gone)

Your machine's /etc/passwd, /etc/group, /etc/myos and /var/lib/myos are
not touched: the test binds copies over them inside the mount namespace.
It does create the empty directories /etc/myos and /var/lib/myos to bind
onto, if they don't exist.

Usage: sudo python3 tests/java_test.py [--no-build]
Needs: cargo, protoc, a JDK 21 (javac, jar, jlink, with jmods), GNU tar,
util-linux unshare/nsenter/setpriv, and protobuf-java: PROTOBUF_JAR=path,
or java/bus/build/protobuf-java.jar (`make -C java/bus fetch-protobuf`),
or /usr/share/java/protobuf.jar (Debian's libprotobuf-java).
"""
import io
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
INIT = ROOT / "rust/init/target/release/myos-init"
BUS_BIN = ROOT / "rust/bus/target/release"
SVC_BIN = ROOT / "rust/services/target/release"
JRUN = SVC_BIN / "jrun"
JAVA = ROOT / "java"
JDK = JAVA / "jdk/build/jdk"
SDK_JAR = JAVA / "sdk/build/myos-sdk.jar"
BUS_JAR = JAVA / "bus/build/myos-bus.jar"
HELLO_PKG = JAVA / "apps/hello/build/org.myos.hello.mapp"
PROBE_DIR = ROOT / "tests/java/probe"
PYPATH = f"{ROOT}/python/bus:{ROOT}/python/admin"

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


def run(*cmd, **kw):
    r = subprocess.run([str(c) for c in cmd], capture_output=True, text=True, **kw)
    return r.returncode, r.stdout + r.stderr


def protobuf_jar():
    for p in (os.environ.get("PROTOBUF_JAR"), JAVA / "bus/build/protobuf-java.jar", "/usr/share/java/protobuf.jar"):
        if p and pathlib.Path(p).exists():
            return pathlib.Path(p).resolve()
    sys.exit("no protobuf-java: set PROTOBUF_JAR, run `make -C java/bus fetch-protobuf`, or apt install libprotobuf-java")


PB = None


def build(no_build):
    global PB
    PB = protobuf_jar()
    env = dict(os.environ)
    env.pop("JAVA_TOOL_OPTIONS", None)
    if not no_build:
        subprocess.run(["make", "-s", "-C", ROOT / "python/bus"], check=True)
        for d in ("rust/init", "rust/bus", "rust/services"):
            if subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT / d).returncode != 0:
                sys.exit(f"cargo build failed in {d}")
        for d in ("bus", "sdk", "apps/hello", "jdk"):
            r = subprocess.run(["make", "-s", "-C", JAVA / d, f"PROTOBUF_JAR={PB}"], env=env, capture_output=True, text=True)
            if r.returncode != 0:
                sys.exit(f"make -C java/{d} failed:\n{r.stdout}{r.stderr}")
    for f in (INIT, BUS_BIN / "busd", SVC_BIN / "logd", SVC_BIN / "sysinfod", JRUN, SDK_JAR, BUS_JAR, HELLO_PKG, JDK / "bin/java"):
        if not f.exists():
            sys.exit(f"{f} missing; run without --no-build")


def make_probe(tmp):
    """Compiles the probe app and packs it with jrun pack."""
    env = dict(os.environ)
    env.pop("JAVA_TOOL_OPTIONS", None)
    d = tmp / "probe"
    (d / "classes").mkdir(parents=True)
    srcs = [str(p) for p in (PROBE_DIR / "src").rglob("*.java")]
    subprocess.run(["javac", "--release", "17", "-nowarn", "-d", d / "classes", "-cp", f"{SDK_JAR}:{BUS_JAR}:{PB}", *srcs],
                   check=True, env=env)
    pkg = d / "pkg"
    pkg.mkdir()
    subprocess.run(["jar", "--create", "--file", pkg / "probe.jar", "-C", d / "classes", "."], check=True, env=env)
    shutil.copy(PROBE_DIR / "app.toml", pkg / "app.toml")
    out = d / "org.myos.probe.mapp"
    code, msg = run(JRUN, "pack", pkg, "-o", out)
    check("jrun pack makes a package", code == 0 and out.exists(), msg)
    return out


def hello_variant(tmp, version, interval):
    """The sample app with a short heartbeat, repacked."""
    d = tmp / f"hello-{version}"
    d.mkdir()
    with tarfile.open(HELLO_PKG) as t:
        t.extractall(d, filter="data")
    text = (d / "app.toml").read_text()
    text = text.replace('version = "1.0.0"', f'version = "{version}"').replace('"--interval", "30"', f'"--interval", "{interval}"')
    (d / "app.toml").write_text(text)
    out = tmp / f"hello-{version}.mapp"
    code, msg = run(JRUN, "pack", d, "-o", out)
    assert code == 0, msg
    return out


# ---------------------------------------------------------------- part 1

def tar_bytes(members, fmt=tarfile.USTAR_FORMAT):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=fmt) as t:
        for name, data, kind in members:
            info = tarfile.TarInfo(name)
            if kind == "file":
                info.size = len(data)
                t.addfile(info, io.BytesIO(data))
            elif kind == "symlink":
                info.type = tarfile.SYMTYPE
                info.linkname = data.decode()
                t.addfile(info)
    return buf.getvalue()


MANIFEST = b'id = "org.example.t"\nversion = "1"\nmain = "a.B"\n'


def test_packages(tmp):
    code, out = run(JRUN, "check", HELLO_PKG)
    check("the sample package is valid", code == 0 and "org.myos.hello 1.0.0" in out and "log, sysinfo" in out, out)
    code, out = run("tar", "tvf", HELLO_PKG)
    check("the package is a plain tar (app.toml + hello.jar)", code == 0 and "app.toml" in out and "hello.jar" in out, out)

    cases = [
        ("path traversal", [("app.toml", MANIFEST, "file"), ("app.jar", b"x", "file"), ("../evil", b"x", "file")], '".." isn\'t allowed'),
        ("absolute path", [("app.toml", MANIFEST, "file"), ("app.jar", b"x", "file"), ("/etc/evil", b"x", "file")], "absolute"),
        ("symlink", [("app.toml", MANIFEST, "file"), ("app.jar", b"/etc/shadow", "symlink")], "links aren't allowed"),
        ("no manifest", [("app.jar", b"x", "file")], "no app.toml"),
        ("jar missing", [("app.toml", MANIFEST, "file")], "isn't in the package"),
        ("bad manifest", [("app.toml", MANIFEST + b'permissions = ["root"]\n', "file"), ("app.jar", b"x", "file")], "unknown permission"),
        ("duplicate", [("app.toml", MANIFEST, "file"), ("app.jar", b"x", "file"), ("./app.jar", b"y", "file")], "appears twice"),
    ]
    for name, members, want in cases:
        p = tmp / "bad.mapp"
        p.write_bytes(tar_bytes(members))
        code, out = run(JRUN, "check", p)
        check(f"jrun check refuses a package with: {name}", code != 0 and want in out, out)
    p = tmp / "gnu.mapp"
    p.write_bytes(tar_bytes([("./app.toml", MANIFEST, "file"), ("./app.jar", b"x", "file")], tarfile.GNU_FORMAT))
    check("GNU-format tar with ./ names is accepted", run(JRUN, "check", p)[0] == 0, run(JRUN, "check", p)[1])
    p = tmp / "gz.mapp"
    import gzip
    p.write_bytes(gzip.compress(tar_bytes([("app.toml", MANIFEST, "file"), ("app.jar", b"x", "file")])))
    code, out = run(JRUN, "check", p)
    check("a gzipped package is refused with a reason", code != 0 and "not a tar" in out, out)
    code, out = run("setpriv", "--reuid", "65534", "--regid", "65534", "--clear-groups", JRUN, "install", HELLO_PKG) \
        if os.geteuid() == 0 else run(JRUN, "install", HELLO_PKG)
    check("jrun install needs root", code != 0 and "needs root" in out, out)
    code, out = run(JRUN, "permissions")
    check("jrun permissions lists them", "network" in out and "call:SERVICE" in out, out)

    env = dict(os.environ)
    env.pop("JAVA_TOOL_OPTIONS", None)
    code, out = run(JDK / "bin/java", "--list-modules", env=env)
    mods = sorted(l.split("@")[0] for l in out.split())
    check("the jlink'd JDK has just the five modules",
          mods == ["java.base", "java.logging", "java.net.http", "jdk.crypto.ec", "jdk.unsupported"], out)
    size = sum(f.stat().st_size for f in JDK.rglob("*") if f.is_file())
    check(f"the jlink'd JDK is small ({size >> 20} MiB)", size < 80 << 20)
    code, out = run(JDK / "bin/java", "-cp", f"{SDK_JAR}:{BUS_JAR}:{PB}", "org.myos.runtime.Main", env=env)
    check("the runtime refuses to start outside jrun", code == 2 and "not running as a myos app" in out, out)


# ---------------------------------------------------------------- part 2

def service(desc, exe, extra=""):
    return f'description = "{desc}"\nexec = ["{exe}"]\nrestart = "always"\nstop_timeout = 2\n{extra}'


def test_system(tmp):
    for tool in ("unshare", "nsenter", "setpriv"):
        if not shutil.which(tool):
            return skip("apps under myos-init", f"needs {tool}")
    if os.geteuid() != 0:
        return skip("apps under myos-init", "needs root: sudo python3 tests/java_test.py")

    probe_pkg = make_probe(tmp)
    hello1 = hello_variant(tmp, "1.0.0", 1)
    hello2 = hello_variant(tmp, "1.0.1", 1)

    etc = tmp / "etc-myos"
    (etc / "services").mkdir(parents=True)
    var = tmp / "var-lib-myos"
    var.mkdir()
    for d in (tmp, etc, var, etc / "services"):
        d.chmod(0o755)
    svc = etc / "services"
    (svc / "busd.toml").write_text(service("IPC bus broker", BUS_BIN / "busd"))
    (svc / "logd.toml").write_text(service("Log daemon", SVC_BIN / "logd",
        '[env]\nMYOS_LOGD_SOCKET = "/run/myos/devlog"\nMYOS_LOGD_KMSG = "none"\n'
        'MYOS_LOGD_FILE = "/var/lib/myos/messages"\nMYOS_LOGD_STATE = "/run/myos/logd.state"\n'))
    (svc / "sysinfo.toml").write_text(service("System information", SVC_BIN / "sysinfod", 'after = ["busd"]\n'))
    (etc / "jrun.toml").write_text(f'java = "{JDK}/bin/java"\nclasspath = ["{SDK_JAR}", "{BUS_JAR}", "{PB}"]\n')
    for f in ("passwd", "group"):
        shutil.copy(f"/etc/{f}", tmp / f)
    console = tmp / "console.log"
    console.touch()

    setup = (f"mkdir -p /etc/myos /var/lib/myos && mount --bind {etc} /etc/myos && mount --bind {var} /var/lib/myos && "
             f"mount --bind {tmp}/passwd /etc/passwd && mount --bind {tmp}/group /etc/group && "
             f"mount --bind {console} /dev/console && exec {INIT}")
    env = dict(os.environ, MYOS_INIT_NO_ROOT_REMOUNT="1", MYOS_INIT_LOG_SOCKET="/run/myos/devlog")
    proc = subprocess.Popen(["unshare", "--fork", "--pid", "--mount", "--uts", "--propagation", "private", "--mount-proc",
                             "sh", "-c", setup], env=env)
    pid = None
    try:
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
            return check("myos-init started in its namespaces", False, console.read_text())

        ns = ("nsenter", "-t", str(pid), "-m", "-p", "-u", "--")
        inside_env = dict(os.environ, PYTHONPATH=PYPATH)
        inside_env.pop("JAVA_TOOL_OPTIONS", None)

        def inside(*cmd, timeout=60):
            r = subprocess.run([*ns, *map(str, cmd)], capture_output=True, text=True, env=inside_env, timeout=timeout)
            return r.returncode, r.stdout + r.stderr

        def status(name):
            return inside(INIT, "ctl", "status", name)[1]

        def py(code):
            return inside(sys.executable, "-c", code)

        def log_entries(tag):
            code, out = py(f"""
from myos.bus import Client
from myos.log.v1 import log_pb2
with Client() as c:
    r = c.call("log", "Query", log_pb2.QueryRequest(tag={tag!r}, limit=1000), log_pb2.QueryReply)
for e in r.entries:
    print(e.source, e.uid, e.priority, e.message.replace(chr(10), " "))
""")
            return out if code == 0 else ""

        check("busd, logd and sysinfod up under myos-init",
              wait_for(lambda: all("running" in status(s) for s in ("busd", "logd", "sysinfo")), 10),
              status("busd") + status("logd") + status("sysinfo") + console.read_text()[-500:])

        # --- install
        code, out = inside(JRUN, "install", hello1, timeout=60)
        check("jrun install the sample app", code == 0 and "installed org.myos.hello 1.0.0" in out, out)
        m = re.search(r"user (\S+) \(uid (\d+)\)", out)
        user, uid = (m.group(1), int(m.group(2))) if m else ("?", -1)
        check("it gets its own user in the app uid range", user == "app_hello" and 20000 <= uid <= 29999, out)
        passwd = (tmp / "passwd").read_text()
        check("/etc/passwd and /etc/group have the user",
              f"app_hello:x:{uid}:{uid}:myos app org.myos.hello:/var/lib/myos/apps/org.myos.hello/data:/bin/false" in passwd
              and f"app_hello:x:{uid}:" in (tmp / "group").read_text(), passwd[-300:])
        data = var / "apps/org.myos.hello/data"
        st = data.stat() if data.exists() else None
        check("its data directory is its own, mode 0700", st and st.st_uid == uid and st.st_mode & 0o777 == 0o700, st)
        pkg = var / "apps/org.myos.hello/pkg"
        check("its package is root's and read-only to it",
              (pkg / "hello.jar").exists() and pkg.stat().st_uid == 0 and (pkg / "app.toml").stat().st_mode & 0o777 == 0o644)
        policy = etc / f"bus/policy.d/{uid}"
        check("busd policy lists what its permissions allow",
              policy.exists() and policy.read_text().splitlines()[1:] == ["app org.myos.hello", "call log", "call sysinfo"],
              policy.read_text() if policy.exists() else "missing")
        sfile = svc / "app.org.myos.hello.toml"
        check("service file written", sfile.exists() and f'exec = ["{JRUN}", "run", "org.myos.hello"]' in sfile.read_text(),
              sfile.read_text() if sfile.exists() else "missing")
        code, out = inside(INIT, "check", "/etc/myos/services")
        check("init accepts the service file", code == 0 and "app.org.myos.hello" in out, out)

        # --- the done-check: it runs as a service and logs through logd
        check("init runs it as the service app.org.myos.hello",
              wait_for(lambda: "running" in status("app.org.myos.hello"), 10), status("app.org.myos.hello"))
        apid = int(re.search(r"pid (\d+)", status("app.org.myos.hello")).group(1))
        code, procstat = inside("cat", f"/proc/{apid}/status")
        uids = re.search(r"^Uid:\s+(\d+)\s+(\d+)\s+(\d+)", procstat, re.M)
        check("the JVM runs as the app's uid (real, effective, saved)", uids and {int(x) for x in uids.groups()} == {uid}, procstat[:400])
        check("with no_new_privs", re.search(r"^NoNewPrivs:\s+1", procstat, re.M), procstat)
        check("and no supplementary groups", re.search(r"^Groups:\s*$", procstat, re.M), procstat)
        code, cmdline = inside("cat", f"/proc/{apid}/cmdline")
        check("it's the trimmed JDK running org.myos.runtime.Main",
              cmdline.startswith(f"{JDK}/bin/java") and "org.myos.runtime.Main" in cmdline and "-Xmx32m" in cmdline,
              cmdline.replace("\0", " "))
        net_app, net_init = inside("readlink", f"/proc/{apid}/ns/net")[1], inside("readlink", "/proc/1/ns/net")[1]
        check("without \"network\" it has a network namespace of its own", net_app.strip() and net_app != net_init,
              net_app + net_init)
        mnt_app, mnt_init = inside("readlink", f"/proc/{apid}/ns/mnt")[1], inside("readlink", "/proc/1/ns/mnt")[1]
        check("and a mount namespace of its own (private /tmp)", mnt_app != mnt_init, mnt_app + mnt_init)

        tag = "app.org.myos.hello"
        check("its SDK log entries reach logd over the bus, with its uid",
              wait_for(lambda: re.search(rf"^bus {uid} 5 hello from Java: org.myos.hello 1.0.0 started \(run 1\)", log_entries(tag), re.M), 15),
              log_entries(tag))
        check("its stdout reaches logd through init", wait_for(lambda: re.search(
            r"^service 0 6 stdout works too", log_entries(tag), re.M), 5), log_entries(tag))
        check("the runtime's start line is logged", re.search(
            r"^service 0 6 starting org.myos.hello 1.0.0 \(org.myos.hello.Hello\), Java 21.*permissions: log, sysinfo", log_entries(tag), re.M),
            log_entries(tag))
        check("heartbeats with figures from sysinfod",
              wait_for(lambda: re.search(rf"^bus {uid} 6 heartbeat 2: load [\d.]+, memory \d+ of \d+ MiB used", log_entries(tag), re.M), 10),
              log_entries(tag))
        check("it noticed it has no network", re.search(r"network: loopback only", log_entries(tag)), log_entries(tag))
        check("it keeps files in its data directory",
              (data / "runs").read_text() == "1\n" and (data / "status").stat().st_uid == uid)

        code, out = py("""
from myos.bus import Client
from google.protobuf import wrappers_pb2
with Client() as c:
    print(c.call("app.org.myos.hello", "Status", wrappers_pb2.StringValue(), wrappers_pb2.StringValue).value)
""")
        check("it serves app.org.myos.hello on the bus", code == 0 and "heartbeats" in out and "asked by uid 0" in out, out)

        code, out = inside(sys.executable, ROOT / "python/admin/myctl", "status", tag)
        check("myctl status shows it", code == 0 and "running" in out, out)
        code, out = inside(sys.executable, ROOT / "python/admin/myctl", "logs", tag, "-n", "100")
        check("myctl logs shows SDK and stdout entries together",
              code == 0 and "hello from Java" in out and "stdout works too" in out, out)
        code, out = inside(JRUN, "list")
        check("jrun list", re.search(r"org.myos.hello\s+1.0.0\s+app_hello\s+running", out), out)
        code, out = inside(JRUN, "info", "org.myos.hello")
        check("jrun info", "loopback only" in out and "bus calls    bus, app.org.myos.hello, log, sysinfo" in out
              and "running" in out, out)

        # --- the probe: what the system refuses
        code, out = inside(JRUN, "install", probe_pkg)
        check("install a second app (not a service)", code == 0 and "not a service" in out and not (svc / "app.org.myos.probe.toml").exists(), out)
        puid = int(re.search(r"uid (\d+)", out).group(1)) if code == 0 else -1
        check("it gets the next uid", puid == uid + 1, out)
        code, out = inside(JRUN, "run", "org.myos.probe", "extra-arg", timeout=60)
        r = dict(l.split(" ", 2)[1:3] for l in out.splitlines() if l.startswith("RESULT ") and l.count(" ") >= 2)
        check("the probe ran to the end", code == 0 and r.get("done") == "yes", out)
        expect = {
            "uid": str(puid), "gid": str(puid), "groups": "none", "no_new_privs": "1",
            "cwd": "/var/lib/myos/apps/org.myos.probe/data", "args": "from-manifest,extra-arg",
            "bus_ping": "ok", "log_write": "ok", "hello_status": "ok",
            "sysinfo_memory": "PERMISSION_DENIED", "net_list": "PERMISSION_DENIED",
            "sdk_sysinfo": "PermissionException", "log_sdk_seq": "ok",
            "register_own": "ok", "register_own_sub": "ok",
            "register_other_app": "PERMISSION_DENIED", "register_system": "PERMISSION_DENIED",
            "interfaces": "lo+", "sdk_connect": "PermissionException", "loopback": "ok",
            "write_own_data": "ok", "sdk_escape": "IllegalArgumentException",
            "read_other_app_data": "AccessDeniedException", "write_etc": "AccessDeniedException",
            "write_own_package": "AccessDeniedException", "write_policy": "AccessDeniedException",
            "write_tmp": "ok",
        }
        for k, v in expect.items():
            check(f"probe: {k} = {v}", r.get(k) == v, f"got {r.get(k)!r}")
        check("probe: connections out fail (no route in its namespace)",
              r.get("connect_outside") in ("SocketException", "NoRouteToHostException", "ConnectException"), r.get("connect_outside"))
        check("probe: its /tmp is private (empty but for its own file)", r.get("tmp_listing") == "probe-tmp-file", r.get("tmp_listing"))
        check("its /tmp file didn't reach the system's /tmp", "probe-tmp-file" not in inside("ls", "-a", "/tmp")[1])
        code, out = py("""
from myos.bus import Client
from myos.log.v1 import log_pb2
with Client() as c:
    r = c.call("log", "Query", log_pb2.QueryRequest(tag="app.org.myos.probe"), log_pb2.QueryReply)
print(sorted((e.uid, e.message) for e in r.entries))
""")
        check("the probe's log entries carry its uid", f"({puid}, 'probe was here')" in out and f"({puid}, 'probe via the SDK')" in out, out)

        code, out = inside("setpriv", "--reuid", "20999", "--regid", "20999", "--clear-groups", sys.executable, "-c", """
from myos.bus import Client, BusError
from myos.log.v1 import log_pb2
with Client() as c:
    print("ping", c.ping("x").message)
    try:
        c.call("log", "Write", log_pb2.WriteRequest(message="x"), log_pb2.WriteReply)
        print("write ok")
    except BusError as e:
        print("write", e)
""")
        check("an app uid without a policy file gets nothing but bus.Ping",
              "ping x" in out and "PERMISSION_DENIED" in out and "has no bus policy" in out, out)

        # --- upgrade
        code, out = inside(JRUN, "install", hello2)
        check("upgrade", code == 0 and "upgraded org.myos.hello 1.0.0 -> 1.0.1" in out and f"(uid {uid})" in out and "new" not in out.split("data in")[0], out)
        check("upgrade restarts it with the new version",
              wait_for(lambda: re.search(r"started \(run 2\)", log_entries(tag)), 15) and "1.0.1 started (run 2)" in log_entries(tag),
              log_entries(tag))
        check("and keeps its data", (data / "runs").read_text() == "2\n")
        check("one user per app, still", (tmp / "passwd").read_text().count("myos app org.myos.hello") == 1)

        # --- stop: SIGTERM reaches the app, which finishes its loop
        inside(INIT, "ctl", "stop", tag)
        check("stop", wait_for(lambda: status(tag).split()[1:2] == ["stopped"], 15), status(tag))
        check("the app shut down cleanly (its own last log line)",
              wait_for(lambda: re.search(r"stopping after \d+ heartbeats", log_entries(tag)), 5), log_entries(tag))
        inside(INIT, "ctl", "start", tag)
        check("start again", wait_for(lambda: "running" in status(tag), 10), status(tag))

        code, out = inside(sys.executable, ROOT / "python/admin/myctl", "disable", tag)
        code, out = inside(JRUN, "install", hello2)
        check("reinstalling keeps a disabled app disabled", "disabled" in out and "enabled = false" in sfile.read_text(), out)
        code, out = inside(JRUN, "install", hello2, "--no-start")
        check("install --no-start", code == 0 and "disabled" in out, out)
        inside(sys.executable, ROOT / "python/admin/myctl", "enable", tag)
        check("myctl enable starts it", wait_for(lambda: "running" in status(tag), 10), status(tag))

        # --- remove
        code, out = inside(JRUN, "remove", "org.myos.hello")
        check("jrun remove", code == 0 and "removed org.myos.hello" in out, out)
        check("its service is gone", "no service" in status(tag) or "unknown" in status(tag) or status(tag).strip() == "" or "error" in status(tag), status(tag))
        check("its files, policy and service file are gone",
              not (var / "apps/org.myos.hello").exists() and not policy.exists() and not sfile.exists())
        check("its user is gone", "org.myos.hello" not in (tmp / "passwd").read_text() and "app_hello" not in (tmp / "group").read_text())
        code, out = inside("ps", "-eo", "uid=")
        check("no process of its uid is left", str(uid) not in out.split(), out)
        code, out = inside(JRUN, "remove", "org.myos.probe", "--keep-data")
        check("remove --keep-data keeps data and user",
              code == 0 and (var / "apps/org.myos.probe/data/probe.txt").exists() and "org.myos.probe" in (tmp / "passwd").read_text(), out)
        code, out = inside(JRUN, "install", probe_pkg)
        check("reinstall after --keep-data reuses the uid", f"(uid {puid})" in out, out)
        code, out = inside(JRUN, "remove", "org.myos.nothing")
        check("removing an app that isn't there fails", code != 0 and "isn't installed" in out, out)
    finally:
        if pid:
            subprocess.run([*("nsenter", "-t", str(pid), "-m", "-p", "-u", "--"), str(INIT), "ctl", "poweroff"],
                           capture_output=True)
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def main():
    no_build = "--no-build" in sys.argv[1:]
    build(no_build)
    # Not under /tmp: init mounts a fresh tmpfs there inside the namespace.
    (JAVA / "build").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="myos-java-test-", dir=JAVA / "build") as t:
        tmp = pathlib.Path(t)
        test_packages(tmp)
        test_system(tmp)
    failed = results.count(False)
    print(f"\n{len(results) - failed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
