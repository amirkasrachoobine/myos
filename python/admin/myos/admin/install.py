"""myos-install: install the running system onto a disk, ready to boot.

  myos-install --list                list the disks
  myos-install [options] DISK        e.g. /dev/vdb: ERASES IT
  myos-install [options] FILE.img    a disk image file (attached as a loop device)

Options:
  -y, --yes            don't ask before erasing DISK
  --hostname NAME      hostname of the installed system (default: this one's)
  --root-size SIZE     size of the root partition, e.g. 2G (default: the whole disk)
  --image-size SIZE    create FILE.img with this size first (e.g. 2G)
  --cmdline ARGS       extra kernel arguments (default: this boot's console= ones)
  --source DIR         what to copy (default: /)
  --kernel FILE        kernel to boot (default: SOURCE/boot/bzImage)
  --grub-dir DIR       GRUB's boot.img and core.img (default: /usr/lib/myos/boot)

What it does (BIOS boot, MBR partition table):
  1. erases the start and end of DISK and writes an MBR with one bootable
     Linux partition from 1 MiB, with GRUB's boot code in it and GRUB's
     core image in the gap before the partition
  2. makes an ext4 filesystem (label myos-root) on the partition
  3. copies the system: everything on SOURCE's filesystem; /proc, /sys,
     /dev, /run, /tmp and /var/log come over empty
  4. writes /boot/grub/grub.cfg, booting root=PARTUUID=<disk id>-01, so the
     installed system boots whatever the disk is called; sets the hostname

Needs root, mkfs.ext4 (e2fsprogs) and mount.
"""

import argparse
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time

from myos.admin import __version__, settings
from myos.admin.copy import Copier, CopyError
from myos.admin.disk import SECTOR, DiskError, Loop, human, parse_size
from myos.admin import disk

PART_START = 2048          # sectors: 1 MiB, the usual alignment; GRUB's core image goes below it
LINUX = 0x83
LABEL = "myos-root"
GRUB_DIR = "/usr/lib/myos/boot"
EMPTY = ["proc", "sys", "dev", "run", "tmp", "mnt", "var/log", "var/tmp"]
SKIP = ["root/.msh_history", "root/.ash_history", "lost+found", "etc/myos/installed"]
# Kernel arguments that describe this boot, not the installed system.
NOT_CARRIED = ("root=", "rootwait", "rootfstype=", "BOOT_IMAGE=", "initrd=", "myos.emergency", "ro", "rw")


class InstallError(Exception):
    pass


def say(msg):
    print(msg, flush=True)


def step(n, msg):
    say(f"[{n}/4] {msg}")


def kernel_args(extra):
    if extra is not None:
        return extra.split()
    try:
        with open("/proc/cmdline") as f:
            words = f.read().split()
    except OSError:
        words = []
    kept = [w for w in words if w.startswith("console=")]
    return kept or ["console=ttyS0"]


def grub_cfg(partuuid, args):
    return f"""\
# Written by myos-install. GRUB reads this from (hd0,msdos1)/boot/grub.
set default=0
set timeout=3
if serial --unit=0 --speed=115200; then
    terminal_input serial console
    terminal_output serial console
fi

menuentry "myos" {{
    linux /boot/bzImage root=PARTUUID={partuuid} rootwait {' '.join(args)}
}}

menuentry "myos (emergency shell)" {{
    linux /boot/bzImage root=PARTUUID={partuuid} rootwait {' '.join(args)} myos.emergency
}}
"""


def cmd_list():
    disks = disk.list_disks()
    if not disks:
        say("no disks found")
        return 0
    say(f"{'DISK':<14} {'SIZE':>10}  {'IN USE':<16} MODEL")
    for dev, size, model, users in disks:
        say(f"{dev:<14} {human(size):>10}  {(', '.join(users) or '-'):<16} {model}")
    return 0


def confirm(target, size, yes):
    if yes:
        return
    if not sys.stdin.isatty():
        raise InstallError(f"refusing to erase {target} without a terminal to confirm on; pass --yes")
    say(f"This ERASES everything on {target} ({human(size)}).")
    try:
        answer = input("Type 'yes' to continue: ")
    except EOFError:
        answer = ""
    if answer.strip() != "yes":
        raise InstallError("not confirmed; nothing was changed")


def current_hostname(source):
    try:
        with open(os.path.join(source, "etc/hostname")) as f:
            return f.read().strip() or "myos"
    except OSError:
        return "myos"


def install(args, dev, size):
    grub_boot = os.path.join(args.grub_dir, "boot.img")
    grub_core = os.path.join(args.grub_dir, "core.img")
    for f in (grub_boot, grub_core):
        if not os.path.isfile(f):
            raise InstallError(f"{f} not found: this system was built without GRUB (BR2_TARGET_GRUB2_I386_PC)")
    kernel = args.kernel or os.path.join(args.source, "boot/bzImage")
    if not os.path.isfile(kernel):
        raise InstallError(f"no kernel at {kernel} (Buildroot: BR2_LINUX_KERNEL_INSTALL_TARGET)")
    mkfs = shutil.which("mkfs.ext4") or shutil.which("mke2fs")
    if not mkfs:
        raise InstallError("mkfs.ext4 not found (e2fsprogs)")
    with open(grub_boot, "rb") as f:
        boot_img = f.read()
    with open(grub_core, "rb") as f:
        core_img = f.read()
    if len(boot_img) != SECTOR:
        raise InstallError(f"{grub_boot} is {len(boot_img)} bytes, not {SECTOR}")
    if len(core_img) > (PART_START - 1) * SECTOR:
        raise InstallError(f"{grub_core} ({human(len(core_img))}) doesn't fit before the partition")

    sectors = size // SECTOR
    root_sectors = sectors - PART_START if args.root_size is None else parse_size(args.root_size) // SECTOR
    if PART_START + root_sectors > sectors:
        raise InstallError(f"--root-size {args.root_size} doesn't fit on {human(size)}")
    copier = Copier(empty=EMPTY, skip=SKIP)
    data = copier.measure(args.source)
    need = int(data * 1.15) + (64 << 20)
    if root_sectors * SECTOR < need:
        raise InstallError(f"{human(root_sectors * SECTOR)} is too small: the system needs about {human(need)}")
    root_sectors -= root_sectors % 8  # whole 4 KiB blocks
    signature = random.SystemRandom().randint(1, 0xFFFFFFFF)
    partuuid = f"{signature:08x}-01"

    step(1, f"partitioning {dev}: MBR, partition 1 = {human(root_sectors * SECTOR)} Linux, bootable; GRUB in the MBR")
    fd = os.open(dev, os.O_RDWR | os.O_SYNC)
    try:
        disk.wipe(fd, size)
        os.pwrite(fd, core_img, SECTOR)
        os.pwrite(fd, disk.mbr([(PART_START, root_sectors, LINUX, True)], signature, boot_img), 0)
        os.fsync(fd)
    finally:
        os.close(fd)
    part = disk.partition_device(dev, 1, PART_START * SECTOR, root_sectors * SECTOR)

    step(2, f"making an ext4 filesystem on {part}")
    r = subprocess.run([mkfs, "-q", "-F", "-t", "ext4", "-L", LABEL, part], capture_output=True, text=True)
    if r.returncode != 0:
        raise InstallError(f"mkfs.ext4 failed: {r.stderr.strip() or r.stdout.strip()}")

    mnt = tempfile.mkdtemp(prefix="myos-install-", dir="/run" if os.path.isdir("/run") else None)
    r = subprocess.run(["mount", "-t", "ext4", part, mnt], capture_output=True, text=True)
    if r.returncode != 0:
        os.rmdir(mnt)
        raise InstallError(f"can't mount {part}: {r.stderr.strip()}")
    try:
        step(3, f"copying {args.source} ({human(data)})")
        t0 = time.monotonic()
        last = [-1]

        def progress(done, total):
            pct = min(100, done * 100 // total)
            if pct // 10 != last[0] // 10 or done == total:
                say(f"      {pct:3d}%  {human(done)}")
                last[0] = pct

        c = copier
        c.progress = progress
        try:
            c.copy(args.source, mnt, total=data)
        except (CopyError, OSError) as e:
            raise InstallError(f"copy failed: {e}") from None
        say(f"      {c.files} files, {human(c.done)} in {time.monotonic() - t0:.0f}s")
        if args.kernel:
            os.makedirs(os.path.join(mnt, "boot"), exist_ok=True)
            shutil.copyfile(kernel, os.path.join(mnt, "boot/bzImage"))

        step(4, f"configuring the installed system (root=PARTUUID={partuuid})")
        grub = os.path.join(mnt, "boot/grub")
        os.makedirs(grub, exist_ok=True)
        with open(os.path.join(grub, "grub.cfg"), "w") as f:
            f.write(grub_cfg(partuuid, kernel_args(args.cmdline)))
        name = args.hostname or current_hostname(args.source)
        saved = os.environ.get("MYOS_SYSROOT")
        os.environ["MYOS_SYSROOT"] = mnt
        try:
            settings.write(settings.plan(sets=[("hostname", name)]))
        finally:
            if saved is None:
                os.environ.pop("MYOS_SYSROOT")
            else:
                os.environ["MYOS_SYSROOT"] = saved
        os.makedirs(os.path.join(mnt, "etc/myos"), exist_ok=True)
        with open(os.path.join(mnt, "etc/myos/installed"), "w") as f:
            f.write(f"installed = {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n"
                    f"installer = myos-install {__version__}\n"
                    f"disk_id = {signature:08x}\n"
                    f"root = PARTUUID={partuuid}\n")
        os.sync()
    finally:
        r = subprocess.run(["umount", mnt], capture_output=True, text=True)
        if r.returncode == 0:
            os.rmdir(mnt)
        else:
            say(f"warning: can't unmount {mnt}: {r.stderr.strip()}")
    say(f"done: {dev} boots myos (hostname {name}). Boot from it, e.g. QEMU's -drive file=DISK,if=virtio,format=raw without -kernel.")


def main(argv=None):
    ap = argparse.ArgumentParser(prog="myos-install", add_help=False)
    ap.add_argument("target", nargs="?")
    ap.add_argument("-y", "--yes", action="store_true")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--hostname")
    ap.add_argument("--root-size")
    ap.add_argument("--image-size")
    ap.add_argument("--cmdline")
    ap.add_argument("--source", default="/")
    ap.add_argument("--kernel")
    ap.add_argument("--grub-dir", default=GRUB_DIR)
    ap.add_argument("-h", "--help", action="store_true")
    ap.add_argument("--version", action="store_true")
    argv = sys.argv[1:] if argv is None else argv
    try:
        args = ap.parse_args(argv)
    except SystemExit:
        return 2
    if args.help:
        print(__doc__.strip())
        return 0
    if args.version:
        print(f"myos-install {__version__} (myos)")
        return 0
    if args.list:
        return cmd_list()
    if not args.target:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    try:
        if args.hostname:
            settings._check_hostname(args.hostname)
        if os.geteuid() != 0:
            raise InstallError("needs root")
        target = args.target
        if args.image_size:
            if os.path.exists(target) and not os.path.isfile(target):
                raise InstallError(f"--image-size: {target} exists and isn't a file")
            with open(target, "wb") as f:
                f.truncate(parse_size(args.image_size))
        if not os.path.exists(target):
            raise InstallError(f"{target} doesn't exist (for an image file, add --image-size 2G)")
        if disk.is_block(target):
            users = disk.in_use(target)
            if users:
                raise InstallError(f"{target} is in use ({', '.join(users)}); it can't be the disk you're running from")
            if os.path.exists(f"{disk._sysfs(target)}/partition"):
                raise InstallError(f"{target} is a partition; give the whole disk")
            size = disk.size_of(target)
            confirm(target, size, args.yes)
            install(args, target, size)
        elif os.path.isfile(target):
            size = disk.size_of(target)
            if not args.image_size:
                confirm(target, size, args.yes)
            with Loop(target) as dev:
                install(args, dev, size)
        else:
            raise InstallError(f"{target} is neither a disk nor a file")
    except (InstallError, DiskError, settings.SettingError) as e:
        print(f"myos-install: {e}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("myos-install: interrupted; the target disk is incomplete", file=sys.stderr)
        return 130
    return 0
