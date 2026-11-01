"""myctl: manage a myos system.

Services (through myos-init's control socket):
  myctl status [SERVICE...]          every service, or the named ones
  myctl start|stop|restart SERVICE...
  myctl enable|disable SERVICE...    start at boot or not; also starts/stops it now
  myctl cat SERVICE                  show its service file
  myctl reload                       re-read /etc/myos/services

Logs (from logd, over the bus):
  myctl logs [SERVICE] [-n N] [-p PRIORITY] [-g TEXT] [-f]

Settings:
  myctl config                       every setting and its value
  myctl get KEY
  myctl set KEY VALUE [KEY VALUE...] [--no-apply]
  myctl unset KEY... [--no-apply]
    keys: hostname, net.IFACE.method (dhcp|static|off), net.IFACE.address,
          net.IFACE.gateway, net.IFACE.dns, service.NAME.enabled;
          unset net.IFACE to stop netd managing an interface

System:
  myctl poweroff|reboot
  myctl version

Services and settings need root; logs show only your own entries otherwise.
"""

import argparse
import datetime
import os
import socket
import sys
import time

from myos.admin import __version__, initctl, settings, sysroot
from myos.admin.initctl import InitError
from myos.admin.settings import SettingError

PRIORITIES = ["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"]
# Exit status of `myctl status NAME` when NAME isn't running (or done), as
# systemctl's is-active uses: scripts can test it.
NOT_ACTIVE = 3


class UsageError(Exception):
    pass


def out(msg=""):
    print(msg, flush=True)


def err(msg):
    print(f"myctl: {msg}", file=sys.stderr, flush=True)


# ---- services ----------------------------------------------------------------

def _descriptions():
    return {name: (s or {}).get("description", "") for name, s in settings.services().items()}


def cmd_status(args):
    rows = initctl.status() if not args.services else [initctl.status(n)[0] for n in args.services]
    desc = _descriptions()
    if not rows:
        out("no services")
        return 0
    w = max(14, *(len(r.name) for r in rows))
    out(f"{'SERVICE':<{w}}  {'STATE':<9} {'DETAIL':<32} DESCRIPTION")
    for r in rows:
        out(f"{r.name:<{w}}  {r.state:<9} {r.detail:<32} {desc.get(r.name, '')}".rstrip())
    if args.services and any(r.state not in ("running", "done") for r in rows):
        return NOT_ACTIVE
    return 0


def _settle(name, timeout=5.0):
    """Waits until `name` is past waiting/stopping, and a oneshot has
    finished (or `timeout` passes); returns its Status."""
    oneshot = (settings.services().get(name) or {}).get("type") == "oneshot"
    busy = ("waiting", "stopping", "running") if oneshot else ("waiting", "stopping")
    end = time.monotonic() + timeout
    while True:
        st = initctl.status(name)[0]
        if st.state not in busy or time.monotonic() >= end:
            return st
        time.sleep(0.1)


def cmd_action(args):
    want = ("stopped", "disabled") if args.command == "stop" else ("running", "done")
    rc = 0
    for name in args.services:
        try:
            initctl.request(args.command, name)
            st = _settle(name)
        except InitError as e:
            err(f"{args.command} {name}: {e}")
            rc = 1
            continue
        out(st.line)
        if st.state not in want:
            rc = 1
    return rc


def cmd_enable(args):
    """Sets `enabled` in each service file, has init re-read them, then makes
    the running state match: started if enabled, stopped if disabled."""
    enable = args.command == "enable"
    known = settings.services()
    for name in args.services:
        if name not in known:
            raise SettingError(f"no service named {name} in {settings.SERVICES}")
    p = settings.plan(sets=[(f"service.{n}.enabled", "true" if enable else "false") for n in args.services])
    settings.write(p)
    if p.reload:
        initctl.request("reload")
    rc = 0
    for name in args.services:
        st = _settle(name)
        if enable and st.state in ("stopped", "failed", "crashed"):
            initctl.request("start", name)
            st = _settle(name)
        elif not enable and st.state not in ("disabled", "stopped", "done", "failed"):
            initctl.request("stop", name)
            st = _settle(name)
        out(st.line)
        if st.state not in (("running", "done") if enable else ("disabled", "stopped", "done", "failed")):
            rc = 1
    return rc


def cmd_cat(args):
    for name in args.services:
        f = settings.path(f"{settings.SERVICES}/{name}.toml")
        try:
            with open(f) as fh:
                text = fh.read()
        except FileNotFoundError:
            raise SettingError(f"no service named {name} in {settings.SERVICES}") from None
        out(f"# {settings.SERVICES}/{name}.toml")
        sys.stdout.write(text if text.endswith("\n") else text + "\n")
    return 0


def cmd_reload(args):
    out(initctl.request("reload").strip())
    return 0


def cmd_power(args):
    out(initctl.request(args.command).strip())
    return 0


# ---- logs --------------------------------------------------------------------

def _priority(s):
    if s.isdigit() and int(s) < 8:
        return int(s)
    if s in PRIORITIES:
        return PRIORITIES.index(s)
    aliases = {"error": 3, "warn": 4, "emergency": 0, "critical": 2}
    if s in aliases:
        return aliases[s]
    raise UsageError(f"unknown priority `{s}` ({' '.join(PRIORITIES)})")


def format_entry(e):
    t = datetime.datetime.fromtimestamp(e.time_us / 1e6, tz=datetime.timezone.utc)
    who = "-" if not e.tag else e.tag if not e.pid else f"{e.tag}[{e.pid}]"
    name = PRIORITIES[e.priority] if e.priority < 8 else str(e.priority)
    return f"{t:%Y-%m-%d %H:%M:%S}.{t.microsecond // 1000:03d} {name:<7} {who}: {e.message}"


def cmd_logs(args):
    from myos.bus import BusError, Client
    from myos.log.v1 import log_pb2

    q = log_pb2.QueryRequest(limit=min(args.lines, 1000), tag=args.service or "", contains=args.grep or "")
    if args.priority is not None:
        q.max_priority = _priority(args.priority)

    def connect():
        try:
            return Client()
        except BusError as e:
            raise BusError(f"{e} (is busd running?)") from None

    def query(bus, req):
        try:
            return bus.call("log", "Query", req, log_pb2.QueryReply)
        except BusError as e:
            if "UNKNOWN_SERVICE" in str(e):
                raise BusError("logd isn't on the bus (myctl status logd)") from None
            raise

    bus = connect()
    if args.lines > 0:
        r = query(bus, q)
        for e in r.entries:
            out(format_entry(e))
        q.after_seq = r.last_seq
    else:
        q.after_seq = query(bus, log_pb2.QueryRequest(limit=1)).last_seq
    if not args.follow:
        return 0
    q.limit = 1000
    down = False
    while True:
        time.sleep(0.5)
        try:
            if bus is None:
                bus = connect()
            r = query(bus, q)
        except BusError as e:
            # logd or busd restarted: say so once, reconnect and carry on.
            if not down:
                err(f"{e}; waiting for it to come back")
                down = True
            if bus is not None:
                bus.close()
                bus = None
            time.sleep(1)
            continue
        if down:
            err("reconnected")
            down = False
        if r.last_seq < q.after_seq:
            q.after_seq = 0  # a new logd counts from 1 again
            continue
        for e in r.entries:
            out(format_entry(e))
        full = len(r.entries) >= q.limit
        q.after_seq = r.entries[-1].seq if full and r.entries else r.last_seq


# ---- settings ----------------------------------------------------------------

def cmd_config(args):
    rows = settings.all_settings()
    w = max(len(k) for k, _ in rows)
    for k, v in rows:
        out(f"{k:<{w}}  {v}")
    return 0


def cmd_get(args):
    for key in args.keys:
        v = settings.get(key)
        out(v if len(args.keys) == 1 else f"{key} {v}")
    return 0


def _apply(p, apply):
    if not p:
        out("no changes")
        return 0
    settings.write(p)
    for f in p.files:
        out(f"wrote {f}")
    for n in p.notes:
        out(f"note: {n}")
    if not apply:
        if p.hostname or p.restart or p.reload:
            out("not applied (--no-apply): takes effect at the next boot"
                + (", or run `myctl restart netd`" if p.restart else "")
                + (", or run `myctl reload`" if p.reload else ""))
        return 0
    rc = 0
    if p.hostname and sysroot() == "/":
        try:
            socket.sethostname(p.hostname)
            out(f"hostname is now {p.hostname}")
        except OSError as e:
            err(f"can't set the running hostname: {e.strerror}; it takes effect at the next boot")
            rc = 1
    if p.reload:
        out(f"init: {initctl.request('reload').strip()}")
    for svc in sorted(p.restart):
        initctl.request("restart", svc)
        st = _settle(svc)
        out(st.line)
        if st.state != "running":
            rc = 1
    return rc


def cmd_set(args):
    if len(args.pairs) % 2:
        raise UsageError("set needs KEY VALUE pairs")
    pairs = list(zip(args.pairs[::2], args.pairs[1::2]))
    return _apply(settings.plan(sets=pairs), args.apply)


def cmd_unset(args):
    return _apply(settings.plan(unsets=args.keys), args.apply)


def cmd_version(args):
    out(f"myctl {__version__} (myos)")
    return 0


# ---- command line ------------------------------------------------------------

class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise UsageError(message)


def parser():
    ap = Parser(prog="myctl", add_help=False, usage="myctl COMMAND [ARGS]  (myctl help)")
    sub = ap.add_subparsers(dest="command", parser_class=Parser)
    p = sub.add_parser("status")
    p.add_argument("services", nargs="*")
    p.set_defaults(fn=cmd_status)
    for name in ("start", "stop", "restart"):
        p = sub.add_parser(name)
        p.add_argument("services", nargs="+")
        p.set_defaults(fn=cmd_action)
    for name in ("enable", "disable"):
        p = sub.add_parser(name)
        p.add_argument("services", nargs="+")
        p.set_defaults(fn=cmd_enable)
    p = sub.add_parser("cat")
    p.add_argument("services", nargs="+")
    p.set_defaults(fn=cmd_cat)
    sub.add_parser("reload").set_defaults(fn=cmd_reload)
    for name in ("poweroff", "reboot"):
        sub.add_parser(name).set_defaults(fn=cmd_power)

    p = sub.add_parser("logs")
    p.add_argument("service", nargs="?")
    p.add_argument("-n", "--lines", type=int, default=50)
    p.add_argument("-p", "--priority")
    p.add_argument("-g", "--grep")
    p.add_argument("-f", "--follow", action="store_true")
    p.set_defaults(fn=cmd_logs)

    sub.add_parser("config").set_defaults(fn=cmd_config)
    p = sub.add_parser("get")
    p.add_argument("keys", nargs="+")
    p.set_defaults(fn=cmd_get)
    p = sub.add_parser("set")
    p.add_argument("pairs", nargs="+")
    p.add_argument("--no-apply", dest="apply", action="store_false")
    p.set_defaults(fn=cmd_set)
    p = sub.add_parser("unset")
    p.add_argument("keys", nargs="+")
    p.add_argument("--no-apply", dest="apply", action="store_false")
    p.set_defaults(fn=cmd_unset)
    sub.add_parser("version").set_defaults(fn=cmd_version)
    return ap


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] in ("help", "-h", "--help"):
        print(__doc__.strip())
        return 0 if argv else 2
    if argv[0] in ("--version", "-V"):
        return cmd_version(None)
    try:
        args = parser().parse_args(argv)
        if not getattr(args, "fn", None):
            raise UsageError(f"unknown command `{argv[0]}`")
        return args.fn(args)
    except UsageError as e:
        err(f"{e}\ntry `myctl help`")
        return 2
    except (InitError, SettingError) as e:
        err(str(e))
        return 1
    except KeyboardInterrupt:
        return 130
    except BrokenPipeError:
        # `myctl logs | head`: stop quietly.
        try:
            sys.stdout = open(os.devnull, "w")
        except OSError:
            pass
        return 0
    except Exception as e:
        from myos.bus import BusError
        if isinstance(e, BusError):
            err(str(e))
            return 1
        raise
