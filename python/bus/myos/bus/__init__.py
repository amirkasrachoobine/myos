"""Python client for the myos IPC bus.

    from myos.bus import Client

    with Client() as bus:
        pong = bus.ping("hello")
        print(pong.message, pong.broker_version)

        # Any service's method, with its generated request/reply classes:
        info = bus.call("sysinfo", "Memory", MemoryRequest(), MemoryReply)

Failures raise BusError (or its subclass BusTimeout); a BusError from the
bus or a service carries a status code in `.code`. The wire format is
described in proto/myos/bus/v1/wire.proto.

The generated message classes are in myos.bus.v1 (wire_pb2, bus_pb2).
"""

import os
import socket
import struct
import time

from myos.bus.v1 import bus_pb2, wire_pb2

__all__ = ["Client", "BusError", "BusTimeout", "Code", "socket_path", "bus_pb2", "wire_pb2"]

DEFAULT_SOCKET = "/run/myos/bus.sock"
DEFAULT_TIMEOUT = 5.0
MAX_FRAME = 16 << 20
_GRACE = 1.0  # the broker is told the same deadline and normally answers first

Code = wire_pb2.Status.Code


def socket_path():
    """$MYOS_BUS_SOCKET, or /run/myos/bus.sock."""
    return os.environ.get("MYOS_BUS_SOCKET") or DEFAULT_SOCKET


class BusError(Exception):
    """A failed call. `code` is a Code value when the bus or the service
    answered with an error, None for connection and protocol problems."""

    def __init__(self, message, code=None):
        super().__init__(message)
        self.code = code


class BusTimeout(BusError):
    """No reply in time."""


class Client:
    """One connection to the bus. One call at a time; open several clients
    for concurrent calls. Usable as a context manager."""

    def __init__(self, path=None, timeout=DEFAULT_TIMEOUT):
        path = path or socket_path()
        self.timeout = timeout
        self._next_id = 0
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self._sock.connect(path)
        except OSError as e:
            self._sock.close()
            self._sock = None
            raise BusError(f"can't connect to {path}: {e.strerror or e}") from e

    def close(self):
        if self._sock is not None:
            self._sock.close()
            self._sock = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def call_raw(self, service, method, payload=b""):
        """Calls service.method with serialized bytes; returns the reply bytes."""
        if self._sock is None:
            raise BusError("connection closed after an earlier error")
        self._next_id += 1
        frame = wire_pb2.Frame(id=self._next_id)
        frame.call.service = service
        frame.call.method = method
        frame.call.payload = payload
        frame.call.timeout_ms = min(int(self.timeout * 1000), 0xFFFFFFFF)
        try:
            self._write(frame)
            deadline = time.monotonic() + self.timeout + _GRACE
            while True:
                reply = self._read(deadline)
                if reply.id == self._next_id:
                    break  # anything else isn't ours; can't normally happen
        except BusError:
            self.close()  # mid-frame or a late reply could follow: don't reuse
            raise
        if reply.WhichOneof("kind") != "reply":
            self.close()
            raise BusError("protocol error: expected a reply")
        status = reply.reply.status
        if status.code != Code.OK:
            name = Code.Name(status.code)
            raise BusError(f"{name}: {status.message}" if status.message else name, code=status.code)
        return reply.reply.payload

    def call(self, service, method, request, reply_type):
        """Calls service.method with a message; returns a `reply_type` message."""
        data = self.call_raw(service, method, request.SerializeToString())
        reply = reply_type()
        try:
            reply.ParseFromString(data)
        except Exception as e:  # DecodeError
            raise BusError(f"{service}.{method} reply didn't decode: {e}") from e
        return reply

    def ping(self, message=""):
        """Round trip through the broker. Returns a bus_pb2.PingReply."""
        return self.call("bus", "Ping", bus_pb2.PingRequest(message=message), bus_pb2.PingReply)

    def list_services(self):
        """Services on the bus, as bus_pb2.ServiceInfo messages."""
        return list(self.call("bus", "ListServices", bus_pb2.ListServicesRequest(), bus_pb2.ListServicesReply).services)

    def _write(self, frame):
        body = frame.SerializeToString()
        if len(body) > MAX_FRAME:
            raise BusError("request too large")
        try:
            self._sock.sendall(struct.pack(">I", len(body)) + body)
        except OSError as e:
            raise BusError(f"bus connection: {e}") from e

    def _read(self, deadline):
        (n,) = struct.unpack(">I", self._read_exact(4, deadline))
        if n > MAX_FRAME:
            raise BusError("protocol error: oversized frame")
        frame = wire_pb2.Frame()
        try:
            frame.ParseFromString(self._read_exact(n, deadline))
        except Exception as e:
            raise BusError(f"protocol error: {e}") from e
        return frame

    def _read_exact(self, n, deadline):
        buf = bytearray()
        while len(buf) < n:
            left = deadline - time.monotonic()
            if left <= 0:
                raise BusTimeout("timed out waiting for a reply")
            self._sock.settimeout(left)
            try:
                chunk = self._sock.recv(n - len(buf))
            except socket.timeout:
                continue  # the loop re-checks the deadline
            except OSError as e:
                raise BusError(f"bus connection: {e}") from e
            if not chunk:
                raise BusError("the broker closed the connection")
            buf += chunk
        return bytes(buf)
