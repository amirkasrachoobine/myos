"""The settings `myctl get/set/unset` manage, and where each one lives.

    hostname                  /etc/hostname (and the 127.0.1.1 line of /etc/hosts)
    net.IFACE.method          /etc/myos/netd.toml: dhcp, static or off
    net.IFACE.address         static only: A.B.C.D/N
    net.IFACE.gateway         static only: A.B.C.D
    net.IFACE.dns             static only: A.B.C.D[,A.B.C.D...]
    service.NAME.enabled      /etc/myos/services/NAME.toml: true or false

Changes are validated as a whole (the rules netd and init apply) before any
file is written. `plan()` returns the new file contents and what has to
happen for them to take effect; myctl writes them and does that.
"""

import ipaddress
import os
import re
import tomllib

from myos.admin import path, tomledit

HOSTNAME = "/etc/hostname"
HOSTS = "/etc/hosts"
NETD = "/etc/myos/netd.toml"
SERVICES = "/etc/myos/services"

NETD_HEADER = """\
# Interfaces netd configures; managed with `myctl set net.IFACE.KEY VALUE`.
# Interfaces not listed are left alone (lo is always 127.0.0.1/8).
"""

_LABEL = re.compile(r"^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$")
_IFACE = re.compile(r"^[A-Za-z0-9_.:-]{1,15}$")
_SERVICE = re.compile(r"^[A-Za-z0-9_.@-]+$")
NET_KEYS = ("method", "address", "gateway", "dns")
STATIC_ONLY = ("address", "gateway", "dns")


class SettingError(Exception):
    pass


def _read(p, default=None):
    try:
        with open(path(p)) as f:
            return f.read()
    except FileNotFoundError:
        if default is None:
            raise SettingError(f"{p} doesn't exist") from None
        return default


def _toml(text, name):
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise SettingError(f"{name}: {e}") from None


def parse_bool(s):
    v = s.strip().lower()
    if v in ("true", "yes", "on", "1"):
        return True
    if v in ("false", "no", "off", "0"):
        return False
    raise SettingError(f"`{s}` is not true or false")


def _split_key(key):
    parts = key.split(".")
    if parts == ["hostname"]:
        return ("hostname",)
    if len(parts) == 3 and parts[0] == "net":
        if not _IFACE.match(parts[1]):
            raise SettingError(f"`{parts[1]}` is not an interface name")
        if parts[2] not in NET_KEYS:
            raise SettingError(f"unknown key `{key}`; net.{parts[1]}.* is one of: {', '.join(NET_KEYS)}")
        return ("net", parts[1], parts[2])
    if len(parts) == 2 and parts[0] == "net":
        if not _IFACE.match(parts[1]):
            raise SettingError(f"`{parts[1]}` is not an interface name")
        return ("net", parts[1], None)
    if len(parts) == 3 and parts[0] == "service" and parts[2] == "enabled":
        if not _SERVICE.match(parts[1]):
            raise SettingError(f"`{parts[1]}` is not a service name")
        return ("service", parts[1], "enabled")
    raise SettingError(f"unknown setting `{key}`; try `myctl config` for the list")


# ---- reading ----------------------------------------------------------------

def hostname():
    return _read(HOSTNAME, "").strip() or "(unset)"


def netd_config():
    """(tables, exists): the parsed netd.toml, and whether the file exists."""
    if not os.path.exists(path(NETD)):
        return {}, False
    return _toml(_read(NETD), NETD), True


def services():
    """{name: parsed service file} for every *.toml in the services dir."""
    out = {}
    d = path(SERVICES)
    try:
        names = sorted(os.listdir(d))
    except FileNotFoundError:
        return out
    for f in names:
        if f.endswith(".toml"):
            try:
                out[f[:-5]] = _toml(_read(f"{SERVICES}/{f}"), f)
            except SettingError:
                out[f[:-5]] = None  # unreadable: init skips it too
    return out


def _fmt(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, list):
        return ",".join(str(x) for x in v)
    return str(v)


def get(key):
    k = _split_key(key)
    if k[0] == "hostname":
        return hostname()
    if k[0] == "net":
        cfg, exists = netd_config()
        if not exists:
            if k[2] in (None, "method"):
                return "dhcp (default: no netd.toml, so DHCP on every Ethernet interface)"
            raise SettingError(f"{key} isn't set")
        table = cfg.get(k[1])
        if table is None:
            raise SettingError(f"{k[1]} isn't in {NETD} (netd leaves it alone)")
        if k[2] is None:
            return " ".join(f"{n}={_fmt(table[n])}" for n in NET_KEYS if n in table)
        if k[2] not in table:
            raise SettingError(f"{key} isn't set")
        return _fmt(table[k[2]])
    svc = services()
    if k[1] not in svc:
        raise SettingError(f"no service named {k[1]} in {SERVICES}")
    if svc[k[1]] is None:
        raise SettingError(f"{SERVICES}/{k[1]}.toml doesn't parse")
    return _fmt(svc[k[1]].get("enabled", True))


def all_settings():
    """[(key, value)] for everything myctl can set, as it is now."""
    out = [("hostname", hostname())]
    cfg, exists = netd_config()
    if not exists:
        out.append(("net.*.method", "dhcp (default: no netd.toml)"))
    for iface, table in cfg.items():
        for n in NET_KEYS:
            if n in table:
                out.append((f"net.{iface}.{n}", _fmt(table[n])))
    for name, s in services().items():
        out.append((f"service.{name}.enabled", "(file doesn't parse)" if s is None else _fmt(s.get("enabled", True))))
    return out


# ---- changing ---------------------------------------------------------------

class Plan:
    """New file contents plus what has to happen for them to take effect."""

    def __init__(self):
        self.files = {}         # absolute path (without sysroot) -> text
        self.hostname = None    # new hostname to set on the running system
        self.restart = set()    # services to restart (netd)
        self.reload = False     # init must re-read the service files
        self.notes = []         # things worth telling the user

    def __bool__(self):
        return bool(self.files)


def _check_hostname(v):
    if len(v) > 64 or not all(_LABEL.match(p) for p in v.split(".")):
        raise SettingError(f"`{v}` is not a valid hostname (letters, digits and '-', at most 63 per dot-separated part)")


def _hosts_with(name, text):
    lines = [l for l in text.splitlines() if not l.startswith("127.0.1.1")]
    lines.append(f"127.0.1.1\t{name}")
    return "\n".join(lines) + "\n"


def _net_value(name, raw):
    if name == "method":
        if raw not in ("dhcp", "static", "off"):
            raise SettingError(f"method must be dhcp, static or off, not `{raw}`")
        return raw
    if name == "address":
        try:
            ipaddress.IPv4Interface(raw)
        except ValueError:
            raise SettingError(f"address `{raw}` is not A.B.C.D/N") from None
        if "/" not in raw:
            raise SettingError(f"address `{raw}` needs a prefix length, e.g. {raw}/24")
        return raw
    if name == "gateway":
        try:
            ipaddress.IPv4Address(raw)
        except ValueError:
            raise SettingError(f"gateway `{raw}` is not an IPv4 address") from None
        return raw
    servers = [s for s in re.split(r"[,\s]+", raw) if s]
    for s in servers:
        try:
            ipaddress.IPv4Address(s)
        except ValueError:
            raise SettingError(f"dns server `{s}` is not an IPv4 address") from None
    return servers


def _validate_netd(tables):
    """The rules netd applies when it loads the file (rust/services/.../netd/config.rs)."""
    for iface, t in tables.items():
        if not isinstance(t, dict):
            raise SettingError(f"{NETD}: `{iface}` must be an interface table")
        m = t.get("method")
        if m is None:
            raise SettingError(f"net.{iface}.method isn't set (dhcp, static or off)")
        if m == "static" and "address" not in t:
            raise SettingError(f"net.{iface}.method static needs net.{iface}.address too, e.g. "
                               f"`myctl set net.{iface}.method static net.{iface}.address 192.168.1.10/24`")


def plan(sets=(), unsets=()):
    """Works out the changes for `sets` [(key, raw value)] and `unsets` [key]."""
    p = Plan()
    netd_text = None
    net_changed = set()
    svc_text = {}

    def netd():
        nonlocal netd_text
        if netd_text is None:
            netd_text = _read(NETD, "") or NETD_HEADER
            if not os.path.exists(path(NETD)):
                p.notes.append(f"created {NETD}: netd now only manages the interfaces listed in it")
        return netd_text

    for key, raw in sets:
        k = _split_key(key)
        if k[0] == "hostname":
            _check_hostname(raw)
            p.files[HOSTNAME] = raw + "\n"
            if os.path.exists(path(HOSTS)):
                p.files[HOSTS] = _hosts_with(raw, _read(HOSTS))
            p.hostname = raw
        elif k[0] == "net":
            if k[2] is None:
                raise SettingError(f"set one key at a time: net.{k[1]}.method, .address, .gateway or .dns")
            value = _net_value(k[2], raw)
            netd_text = tomledit.set_value(netd(), k[1], k[2], value)
            if k[2] == "method" and value != "static":
                for extra in STATIC_ONLY:
                    before = netd_text
                    netd_text = tomledit.delete_key(netd_text, k[1], extra)
                    if netd_text != before:
                        p.notes.append(f"removed net.{k[1]}.{extra} (only used with method static)")
            net_changed.add(k[1])
        else:
            name = k[1]
            f = f"{SERVICES}/{name}.toml"
            if not os.path.exists(path(f)):
                raise SettingError(f"no service named {name} in {SERVICES}")
            text = svc_text.get(f) or _read(f)
            svc_text[f] = tomledit.set_value(text, None, "enabled", parse_bool(raw))

    for key in unsets:
        k = _split_key(key)
        if k[0] == "hostname":
            raise SettingError("hostname can't be unset")
        if k[0] == "service":
            raise SettingError(f"use `myctl set {key} true` (services are enabled unless set to false)")
        if not os.path.exists(path(NETD)) and netd_text is None:
            raise SettingError(f"{NETD} doesn't exist; nothing to unset")
        if k[2] is None:
            netd_text = tomledit.delete_table(netd(), k[1])
        elif k[2] == "method":
            raise SettingError(f"to stop netd managing {k[1]}, unset the whole interface: `myctl unset net.{k[1]}`")
        else:
            netd_text = tomledit.delete_key(netd(), k[1], k[2])
        net_changed.add(k[1])

    if netd_text is not None:
        tables = _toml(netd_text, NETD)
        _validate_netd(tables)
        if netd_text != _read(NETD, ""):
            p.files[NETD] = netd_text
            p.restart.add("netd")
    for f, text in svc_text.items():
        if text != _read(f):
            p.files[f] = text
            p.reload = True
    return p


def write(p):
    """Writes the plan's files (atomically, each one)."""
    for f, text in p.files.items():
        full = path(f)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        tomledit.write_atomic(full, text)
