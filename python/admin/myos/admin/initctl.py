"""Client for myos-init's control socket (the protocol `initctl` speaks).

One request line in, a text reply out, then the connection closes. A reply
starting with "error:" is a failure. The socket is root-only.

    status [NAME]  start|stop|restart NAME  reload  poweroff|reboot|halt
"""

import os
import socket

DEFAULT_SOCKET = "/run/myos/init.sock"


class InitError(Exception):
    pass


def socket_path():
    """$MYOS_INIT_SOCKET, or /run/myos/init.sock."""
    return os.environ.get("MYOS_INIT_SOCKET") or DEFAULT_SOCKET


def request(*words, timeout=5.0):
    """Sends one command; returns the reply text. Raises InitError for an
    "error:" reply or when init can't be reached."""
    path = socket_path()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        try:
            s.connect(path)
        except PermissionError:
            raise InitError(f"can't reach init at {path}: permission denied (it's root-only)") from None
        except OSError as e:
            raise InitError(f"can't reach init at {path}: {e.strerror or e}") from None
        s.sendall((" ".join(words) + "\n").encode())
        s.shutdown(socket.SHUT_WR)
        chunks = []
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            chunks.append(chunk)
    except socket.timeout:
        raise InitError("init didn't answer in time") from None
    finally:
        s.close()
    reply = b"".join(chunks).decode(errors="replace")
    if reply.startswith("error:"):
        raise InitError(reply[len("error:"):].strip())
    return reply


class Status:
    """One line of `status`: name, state ("running", "stopped", "disabled",
    "done", "failed", "crashed", "waiting", "stopping") and the rest."""

    def __init__(self, line):
        parts = line.split(None, 2)
        self.name = parts[0] if parts else ""
        self.state = parts[1] if len(parts) > 1 else ""
        self.detail = parts[2].strip() if len(parts) > 2 else ""
        self.line = line

    def __repr__(self):
        return f"Status({self.line!r})"


def status(name=None):
    """A list of Status, one per service (or just `name`)."""
    reply = request("status", name) if name else request("status")
    if reply.startswith("no services in "):
        return []
    return [Status(l) for l in reply.splitlines() if l.strip()]
