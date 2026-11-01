#!/usr/bin/env python3
"""Phase 2 host test: build busd and the four bus clients on this machine,
start busd on a private socket, and check that each client can ping it.
No QEMU or Buildroot needed, so it runs in seconds.

Needs: cargo, protoc, cmake, pkg-config, the C++ protobuf library, the Python
protobuf module, and for Java a JDK 17+ plus protobuf-java (fetched from
Maven Central into java/bus/build/ if missing; Java is skipped otherwise).
On Debian/Ubuntu:
    apt install protobuf-compiler libprotobuf-dev python3-protobuf cmake pkg-config openjdk-21-jdk-headless

Usage: python3 tests/bus_test.py
"""
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
RUST = ROOT / "rust/bus"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""), flush=True)


def skip(name, why):
    print(f"SKIP  {name}  ({why})", flush=True)


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def need(*tools):
    missing = [t for t in tools if not shutil.which(t)]
    if missing:
        sys.exit(f"missing tools: {' '.join(missing)} (see the docstring at the top of {__file__})")


def build(tmp):
    """Builds everything; returns {language: ping command} for what built."""
    clients = {}
    print("building Rust (cargo test)...", flush=True)
    r = run(["cargo", "test", "--release", "-q"], cwd=RUST)
    check("rust: cargo test", r.returncode == 0, r.stdout.strip().splitlines()[-1] if r.returncode == 0 else r.stderr[-2000:])
    r = run(["cargo", "build", "--release", "-q"], cwd=RUST)
    if r.returncode != 0:
        sys.exit(r.stderr)
    clients["Rust"] = [str(RUST / "target/release/myos-bus"), "ping"]

    print("building C++...", flush=True)
    cb = tmp / "cpp"
    r = run(["cmake", "-S", ROOT / "cpp/bus", "-B", cb])
    if r.returncode == 0:
        r = run(["cmake", "--build", cb, "-j", str(os.cpu_count() or 2)])
    check("c++: build", r.returncode == 0, "" if r.returncode == 0 else (r.stdout + r.stderr)[-2000:])
    if r.returncode == 0:
        clients["C++"] = [str(cb / "myos-bus-ping-cpp")]

    print("building Python...", flush=True)
    pb = tmp / "python"
    shutil.copytree(ROOT / "python/bus", pb, ignore=shutil.ignore_patterns("v1", "__pycache__"))
    r = run(["make", "-C", pb, "all", f"PROTO_DIR={ROOT / 'proto'}"])
    check("python: generate", r.returncode == 0, "" if r.returncode == 0 else r.stderr[-2000:])
    if r.returncode == 0:
        os.environ["PYTHONPATH"] = str(pb)
        clients["Python"] = [sys.executable, str(pb / "myos-bus-ping-py")]

    print("building Java...", flush=True)
    jdir = ROOT / "java/bus"
    if not (shutil.which("javac") and shutil.which("jar")):
        skip("java", "no JDK (javac/jar) on PATH")
    else:
        pbjar = jdir / "build/protobuf-java.jar"
        if not pbjar.exists():
            r = run(["make", "-C", jdir, "fetch-protobuf"])
            if r.returncode != 0:
                skip("java", "couldn't download protobuf-java; pass a jar with "
                     "`make -C java/bus PROTOBUF_JAR=...` or run `make -C java/bus fetch-protobuf`")
        if pbjar.exists():
            r = run(["make", "-C", jdir, "all"])
            check("java: build", r.returncode == 0, "" if r.returncode == 0 else (r.stdout + r.stderr)[-2000:])
            if r.returncode == 0:
                clients["Java"] = ["java", "-cp", f"{jdir / 'build/myos-bus.jar'}:{pbjar}", "org.myos.bus.Ping"]
    return clients


def main():
    need("cargo", "protoc", "cmake", "pkg-config", "make")
    with tempfile.TemporaryDirectory(prefix="myos-bus-") as t:
        tmp = pathlib.Path(t)
        clients = build(tmp)

        sock = tmp / "run/bus.sock"
        os.environ["MYOS_BUS_SOCKET"] = str(sock)
        busd = subprocess.Popen([RUST / "target/release/busd"], stderr=subprocess.PIPE, text=True)
        try:
            for _ in range(100):
                if sock.exists():
                    break
                time.sleep(0.05)
            check("busd: listening", sock.exists(), str(sock))

            # The Phase 2 done-check: each language pings and gets its message back.
            for lang, cmd in clients.items():
                r = run(cmd)
                want = f'pong: "hello from {lang}"'
                check(f"{lang.lower()}: ping", r.returncode == 0 and want in r.stdout,
                      (r.stdout + r.stderr).strip())

            # Messages survive the trip byte-for-byte in every language.
            odd = 'tabs\tand "quotes" and ünïcödé'
            for lang, cmd in clients.items():
                r = run(cmd + [odd])
                check(f"{lang.lower()}: ping round-trips unicode", r.returncode == 0 and "ünïcödé" in r.stdout,
                      r.stdout.strip() or r.stderr.strip())

            # Clients fail cleanly (exit 1, message on stderr) without a broker.
            env = dict(os.environ, MYOS_BUS_SOCKET=str(tmp / "nope.sock"))
            for lang, cmd in clients.items():
                r = run(cmd, env=env)
                check(f"{lang.lower()}: no broker -> exit 1", r.returncode == 1 and "nope.sock" in r.stderr,
                      r.stderr.strip())

            if "Python" in clients:
                sys.path.insert(0, os.environ["PYTHONPATH"])
                from myos.bus import BusError, Client, Code

                with Client() as bus:
                    names = [s.name for s in bus.list_services()]
                    check("python: list_services", names == ["bus"], str(names))
                    try:
                        bus.call_raw("nobody", "Ping")
                        check("python: unknown service -> UNKNOWN_SERVICE", False)
                    except BusError as e:
                        check("python: unknown service -> UNKNOWN_SERVICE", e.code == Code.UNKNOWN_SERVICE, str(e))
                    check("python: connection usable after an error", bus.ping("again").message == "again")
        finally:
            busd.terminate()
            busd.wait()

    failed = results.count(False)
    print(f"\n{len(results) - failed} passed, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
