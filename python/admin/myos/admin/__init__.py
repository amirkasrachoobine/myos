"""myos admin tools (Phase 5): myctl and myos-install.

    myos.admin.initctl    talk to PID 1 (myos-init) over its control socket
    myos.admin.tomledit   edit TOML files in place, keeping their comments
    myos.admin.settings   the settings `myctl get/set` knows about
    myos.admin.myctl      the myctl command
    myos.admin.disk       MBR partition tables, block devices, GRUB's boot sectors
    myos.admin.copy       copy a directory tree, one filesystem, keeping metadata
    myos.admin.install    the myos-install command

Every path the tools touch goes through sysroot(), so tests can point them
at a scratch directory with $MYOS_SYSROOT instead of the real /.
"""

import os

__version__ = "0.5.0"


def sysroot():
    """$MYOS_SYSROOT, or "/"."""
    return os.environ.get("MYOS_SYSROOT") or "/"


def path(p):
    """`p` (an absolute path such as /etc/hostname) under sysroot()."""
    return os.path.join(sysroot(), p.lstrip("/"))
