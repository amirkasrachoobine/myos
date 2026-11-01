#!/usr/bin/env python3
"""Fast host-side test of myos-init: no Buildroot build, no QEMU.

Runs the init you built with `cargo build --release` as PID 1 of fresh PID,
mount and UTS namespaces on your own machine, with a throwaway set of test
services, then drives it through initctl and checks what happens.

Needs root (namespaces + mounts) and util-linux's unshare/nsenter.
Your real root filesystem is not remounted (MYOS_INIT_NO_ROOT_REMOUNT).

Usage: sudo python3 tests/init_ns_test.py [path/to/myos-init]
"""
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
INIT = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "rust/init/target/release/myos-init").resolve()

SERVICES = {
    "sleeper.toml": """
        description = "long-running service we crash and stop"
        exec = ["/bin/sleep", "1000"]
        restart = "always"
    """,
    "after-sleeper.toml": """
        type = "oneshot"
        exec = ["/bin/sh", "-c", "echo ran > $OUT"]
        after = ["sleeper"]
        [env]
        OUT = "{work}/after-ran"
    """,
    "crasher.toml": """
        exec = ["/bin/false"]
        restart = "on-failure"
    """,
    "orphan-maker.toml": """
        # leaves a child behind that gets reparented to PID 1
        type = "oneshot"
        exec = ["/bin/sh", "-c", "sleep 0.3 & exit 0"]
    """,
    "missing-binary.toml": """
        type = "oneshot"
        exec = ["/nonexistent/program"]
    """,
    "off.toml": """
        exec = ["/bin/sleep", "1000"]
        enabled = false
    """,
    "broken.toml": """
        exec = "/bin/true"
    """,
}

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + ("" if ok or not detail else f"\n      {detail}"))


def main():
    if os.geteuid() != 0:
        sys.exit("needs root: sudo python3 tests/init_ns_test.py")
    if not INIT.exists():
        sys.exit(f"{INIT} not found; run `cargo build --release` in rust/init first")

    # Not under /tmp: init mounts a fresh tmpfs there inside the namespace.
    work = pathlib.Path(tempfile.mkdtemp(prefix="myos-init-test-", dir=ROOT / "rust/init/target"))
    svc_dir = work / "services"
    svc_dir.mkdir()
    for fname, body in SERVICES.items():
        text = "\n".join(line.strip() for line in body.format(work=work).splitlines())
        (svc_dir / fname).write_text(text + "\n")
    (work / "hostname").write_text("nstest\n")
    console = work / "console.log"
    console.touch()

    # Bind the test files over the real paths, inside the new mount namespace only.
    setup = (
        f"mkdir -p /etc/myos/services && mount --bind {svc_dir} /etc/myos/services && "
        f"{{ [ -e /etc/hostname ] && mount --bind {work}/hostname /etc/hostname || true; }} && "
        f"mount --bind {console} /dev/console && exec {INIT}"
    )
    env = dict(os.environ, MYOS_INIT_NO_ROOT_REMOUNT="1")
    proc = subprocess.Popen(
        ["unshare", "--fork", "--pid", "--mount", "--uts", "--propagation", "private", "--mount-proc", "sh", "-c", setup],
        env=env,
    )

    def init_pid():
        for _ in range(50):
            try:
                kids = pathlib.Path(f"/proc/{proc.pid}/task/{proc.pid}/children").read_text().split()
                if kids and os.readlink(f"/proc/{kids[0]}/exe") == str(INIT):
                    return int(kids[0])
            except OSError:
                pass
            time.sleep(0.1)
        raise RuntimeError("init didn't start")

    pid = init_pid()

    def inside(*cmd):
        r = subprocess.run(["nsenter", "-t", str(pid), "-m", "-p", "-u", "--", *cmd], capture_output=True, text=True)
        return r.returncode, r.stdout + r.stderr

    def ctl(*args):
        return inside(str(INIT), "ctl", *args)

    def status(name):
        return ctl("status", name)[1]

    def wait_for(fn, timeout=5):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if fn():
                return True
            time.sleep(0.1)
        return fn()

    log = lambda: console.read_text()

    try:
        check("init is PID 1 in its namespace", wait_for(lambda: "running" in status("sleeper")), status("sleeper"))
        check("hostname set from /etc/hostname", inside("hostname")[1].strip() == "nstest", inside("hostname")[1])
        check("broken service file skipped with a reason", "broken.toml: `exec` must be a list of strings" in log(), log())

        check("oneshot ran after its dependency", wait_for(lambda: (work / "after-ran").exists()))
        lines = log().splitlines()
        first = lambda s: next((i for i, l in enumerate(lines) if s in l), 10**9)
        check("dependency order respected", first("started sleeper") < first("started after-sleeper"), log())
        check("oneshot status is done", "done      exit code 0" in status("after-sleeper"), status("after-sleeper"))

        check("missing binary reported as failed",
              wait_for(lambda: "failed    cannot run /nonexistent/program" in status("missing-binary")),
              status("missing-binary"))
        check("disabled service not started", "disabled" in status("off"), status("off"))
        check("crashing service backs off and retries",
              wait_for(lambda: "restarts 2" in status("crasher"), timeout=6), status("crasher"))

        code, out = ctl("status", "sleeper")
        old_pid = int(out.split("pid ")[1].split(",")[0])
        inside("kill", "-9", str(old_pid))
        check("killed service restarted after backoff",
              wait_for(lambda: "running" in status("sleeper") and "restarts 1" in status("sleeper")),
              status("sleeper"))

        ctl("stop", "sleeper")
        check("stop", wait_for(lambda: status("sleeper").split()[1] == "stopped"), status("sleeper"))
        time.sleep(1.5)
        check("stopped service stays stopped", status("sleeper").split()[1] == "stopped", status("sleeper"))
        ctl("start", "sleeper")
        check("start", wait_for(lambda: "running" in status("sleeper")), status("sleeper"))

        ctl("restart", "sleeper")
        check("restart", wait_for(lambda: "running" in status("sleeper") and f"pid {old_pid}," not in status("sleeper")),
              status("sleeper"))

        def zombies():
            code, out = inside("sh", "-c", "grep -l '^State:.*Z' /proc/[0-9]*/status 2>/dev/null | wc -l")
            return out.strip()
        check("orphans reaped (no zombies)", wait_for(lambda: zombies() == "0"), f"zombies: {zombies()}")

        code, out = ctl("frobnicate")
        check("unknown command is an error", code == 1 and out.startswith("error:"), out)
        code, out = ctl("status", "nope")
        check("unknown service is an error", code == 1 and "no service named nope" in out, out)

        (svc_dir / "late.toml").write_text('exec = ["/bin/sleep", "1000"]\n')
        (svc_dir / "crasher.toml").unlink()
        code, out = ctl("reload")
        check("reload reports changes", "added: late" in out and "removed: crasher" in out, out)
        check("reload starts new service", wait_for(lambda: "running" in status("late")), status("late"))
        check("reload forgets removed service", wait_for(lambda: "no service named crasher" in status("crasher")),
              status("crasher"))

        # BusyBox `poweroff` sends SIGUSR2 to PID 1; do exactly that.
        inside("kill", "-USR2", "1")
        try:
            proc.wait(timeout=15)
            exited = True
        except subprocess.TimeoutExpired:
            exited = False
        check("poweroff signal shuts down", exited and "powering off" in log().splitlines()[-1], log()[-600:])
        check("services stopped before poweroff", "sleeper stopped" in log() and "late stopped" in log(), log()[-600:])
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        print(f"\nconsole log: {console}")

    if all(results):
        print(f"all {len(results)} checks passed")
        shutil.rmtree(work, ignore_errors=True)
        return 0
    print(f"{results.count(False)} of {len(results)} checks failed")
    return 1


if __name__ == "__main__":
    sys.exit(main())
