"""The Control Center's guest-root service and local package operations."""

import os
from pathlib import Path
import re
import shutil
import stat
import subprocess

from myos.admin import initctl

SERVICE_NAME = re.compile(r"[A-Za-z0-9_.-]+")
APP_ID = re.compile(r"[a-z][a-z0-9-]*(?:\.[a-z][a-z0-9-]*)+")
SERVICE_ACTIONS = {"s": "start", "t": "stop", "x": "restart"}
# Same archive limit as jrun's load_package.
MAX_PACKAGE_BYTES = 272 << 20


def jrun(*args, timeout=30):
    executable = shutil.which("jrun")
    if executable is None:
        raise ValueError("jrun is missing. Boot a MyOS image built with Java enabled.")
    result = subprocess.run([executable, *args], capture_output=True, text=True,
                            errors="replace", timeout=timeout)
    output = "\n".join(s.strip() for s in (result.stdout, result.stderr) if s.strip())
    if result.returncode:
        raise ValueError(output or f"jrun {args[0]} exited with status {result.returncode}")
    return output


def preview(path, staged):
    """Copy the package into a private directory before validating its bytes."""
    source = Path(path)
    if not source.is_absolute() or source.suffix != ".mapp":
        raise ValueError("Enter an absolute guest path ending in .mapp.")
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC
    with os.fdopen(os.open(source, flags), "rb") as package:
        info = os.fstat(package.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise ValueError("Choose a regular .mapp file.")
        if info.st_size > MAX_PACKAGE_BYTES:
            raise ValueError("The app package exceeds the 272 MiB archive limit.")
        with staged.open("wb") as copy:
            remaining = MAX_PACKAGE_BYTES
            while remaining:
                block = package.read(min(1 << 20, remaining))
                if not block:
                    break
                copy.write(block)
                remaining -= len(block)
            if package.read(1):
                raise ValueError("The app package exceeds the 272 MiB archive limit.")
    try:
        return jrun("check", str(staged))
    except ValueError as error:
        raise ValueError(str(error).replace(str(staged), path)) from None


def apply(action, target):
    if os.geteuid() != 0:
        raise ValueError("System actions require guest root.")
    if action in SERVICE_ACTIONS.values() and SERVICE_NAME.fullmatch(target):
        return initctl.request(action, target).strip()
    if action == "install":
        return jrun("install", str(target))
    if action == "remove" and APP_ID.fullmatch(target) and len(target) <= 60:
        return jrun("remove", target, "--keep-data")
    raise ValueError("Invalid action or target.")
