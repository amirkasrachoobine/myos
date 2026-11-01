"""Disks: listing them, MBR partition tables, loop devices, GRUB's boot code.

Nothing here needs fdisk, sfdisk, losetup or grub-install: the partition
table is written directly, the kernel is asked to re-read it with the
BLKRRPART ioctl, and image files are attached with the loop ioctls.
"""

import errno
import fcntl
import os
import stat
import struct
import time

SECTOR = 512
BLKRRPART = 0x125F
BLKPG = 0x1269
BLKGETSIZE64 = 0x80081272
LOOP_SET_FD = 0x4C00
LOOP_CLR_FD = 0x4C01
LOOP_SET_STATUS64 = 0x4C04
LOOP_CTL_GET_FREE = 0x4C82
LO_FLAGS_AUTOCLEAR = 4
LO_FLAGS_PARTSCAN = 8


class DiskError(Exception):
    pass


# ---- sizes ------------------------------------------------------------------

def parse_size(s):
    """'2G', '512M', '1048576' -> bytes (powers of 1024)."""
    units = {"": 1, "K": 1 << 10, "M": 1 << 20, "G": 1 << 30, "T": 1 << 40}
    t = s.strip().upper().removesuffix("B").removesuffix("I")
    num, unit = (t[:-1], t[-1]) if t and t[-1] in units else (t, "")
    try:
        n = float(num)
    except ValueError:
        raise DiskError(f"`{s}` is not a size (e.g. 2G, 512M)") from None
    if n <= 0:
        raise DiskError(f"`{s}` is not a size (e.g. 2G, 512M)")
    return int(n * units[unit])


def human(n):
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if n < 1024 or unit == "TiB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024


def size_of(dev):
    """Size in bytes of a block device or file."""
    st = os.stat(dev)
    if stat.S_ISREG(st.st_mode):
        return st.st_size
    with open(dev, "rb") as f:
        buf = fcntl.ioctl(f.fileno(), BLKGETSIZE64, b"\0" * 8)
    return struct.unpack("Q", buf)[0]


# ---- block devices ----------------------------------------------------------

def is_block(p):
    try:
        return stat.S_ISBLK(os.stat(p).st_mode)
    except FileNotFoundError:
        return False


def _sysfs(dev):
    """/sys/class/block/<name> for a /dev node."""
    return f"/sys/class/block/{os.path.basename(os.path.realpath(dev))}"


def _read(p):
    try:
        with open(p) as f:
            return f.read().strip()
    except OSError:
        return ""


def devnum(dev):
    """'maj:min' of a device node."""
    rdev = os.stat(dev).st_rdev
    return f"{os.major(rdev)}:{os.minor(rdev)}"


def partition_node(dev, n):
    """/dev/vdb -> /dev/vdb1; /dev/loop0, /dev/nvme0n1 -> ...p1."""
    return f"{dev}p{n}" if dev[-1].isdigit() else f"{dev}{n}"


def partitions(dev):
    """[(name, 'maj:min')] of the partitions the kernel knows on `dev`."""
    d = _sysfs(dev)
    out = []
    try:
        for name in sorted(os.listdir(d)):
            if os.path.exists(f"{d}/{name}/partition"):
                out.append((name, _read(f"{d}/{name}/dev")))
    except FileNotFoundError:
        pass
    return out


def in_use(dev):
    """Mount points (and 'swap') using `dev` or any of its partitions."""
    nums = {devnum(dev)} | {num for _, num in partitions(dev)}
    users = []
    try:
        with open("/proc/self/mountinfo") as f:
            for line in f:
                fields = line.split()
                if fields[2] in nums:
                    users.append(fields[4])
    except FileNotFoundError:
        pass
    try:
        with open("/proc/swaps") as f:
            for line in list(f)[1:]:
                p = line.split()[0]
                if os.path.exists(p) and is_block(p) and devnum(p) in nums:
                    users.append("swap")
    except FileNotFoundError:
        pass
    return users


def list_disks():
    """[(dev, size, model, users)] for every whole disk in /sys/block,
    leaving out empty loop devices, RAM disks and the like."""
    out = []
    try:
        names = sorted(os.listdir("/sys/block"))
    except FileNotFoundError:
        return out
    for name in names:
        if name.startswith(("ram", "zram", "dm-", "sr", "fd")):
            continue
        sectors = int(_read(f"/sys/block/{name}/size") or 0)
        if sectors == 0:
            continue
        dev = f"/dev/{name}"
        model = _read(f"/sys/block/{name}/device/model") or _read(f"/sys/block/{name}/loop/backing_file")
        users = in_use(dev) if os.path.exists(dev) else []
        out.append((dev, sectors * SECTOR, model, users))
    return out


def _blkpg_add(dev, n, start, length):
    """Tells the kernel about partition `n` directly (BLKPG_ADD_PARTITION),
    as `partx -a` does: for kernels that can't parse the table themselves."""
    import ctypes

    class Partition(ctypes.Structure):
        _fields_ = [("start", ctypes.c_longlong), ("length", ctypes.c_longlong), ("pno", ctypes.c_int),
                    ("devname", ctypes.c_char * 64), ("volname", ctypes.c_char * 64)]

    class Arg(ctypes.Structure):
        _fields_ = [("op", ctypes.c_int), ("flags", ctypes.c_int), ("datalen", ctypes.c_int), ("data", ctypes.c_void_p)]

    part = Partition(start=start, length=length, pno=n)
    arg = Arg(op=1, flags=0, datalen=ctypes.sizeof(part), data=ctypes.cast(ctypes.pointer(part), ctypes.c_void_p))
    fd = os.open(dev, os.O_RDONLY)
    try:
        fcntl.ioctl(fd, BLKPG, arg)
    except OSError as e:
        if e.errno != errno.EBUSY:  # EBUSY: it's there already
            raise DiskError(f"can't add partition {n} of {dev}: {e.strerror}") from None
    finally:
        os.close(fd)


def partition_device(dev, n, start, length, timeout=5.0):
    """The /dev node of partition `n` (at byte `start`, `length` bytes) after
    its table was written: asks the kernel to re-read the table, adds the
    partition by hand if the kernel can't parse MBRs, and makes the node
    from sysfs if devtmpfs doesn't."""
    reread_partitions(dev)
    node = partition_node(dev, n)
    sysdir = f"{_sysfs(dev)}/{os.path.basename(node)}"
    added = False
    end = time.monotonic() + timeout
    while True:
        if is_block(node):
            return node
        num = _read(f"{sysdir}/dev")
        if num and time.monotonic() > end - timeout / 2:
            major, minor = (int(x) for x in num.split(":"))
            try:
                os.mknod(node, 0o600 | stat.S_IFBLK, os.makedev(major, minor))
            except FileExistsError:
                pass
            return node
        if not num and not added and time.monotonic() > end - timeout * 0.8:
            _blkpg_add(dev, n, start, length)
            added = True
        if time.monotonic() >= end:
            raise DiskError(f"{node} didn't appear after re-reading the partition table")
        time.sleep(0.1)


def reread_partitions(dev):
    """Asks the kernel to re-read `dev`'s partition table (retrying while busy)."""
    fd = os.open(dev, os.O_RDONLY)
    try:
        for attempt in range(20):
            try:
                fcntl.ioctl(fd, BLKRRPART)
                return
            except OSError as e:
                if e.errno != errno.EBUSY or attempt == 19:
                    raise DiskError(f"the kernel can't re-read {dev}'s partitions: {e.strerror}") from None
                time.sleep(0.25)
    finally:
        os.close(fd)


# ---- loop devices -----------------------------------------------------------

class Loop:
    """An image file attached to a free loop device, with partition scanning.

        with Loop("disk.img") as dev:   # "/dev/loop0"
            ...
    """

    def __init__(self, image):
        self.image = image
        self.dev = None
        self._fd = None

    def __enter__(self):
        try:
            ctl = os.open("/dev/loop-control", os.O_RDWR)
        except OSError as e:
            raise DiskError(f"can't attach {self.image}: /dev/loop-control: {e.strerror} (needs root and CONFIG_BLK_DEV_LOOP)") from None
        try:
            for _ in range(10):
                n = fcntl.ioctl(ctl, LOOP_CTL_GET_FREE)
                dev = f"/dev/loop{n}"
                if not os.path.exists(dev):
                    num = _read(f"/sys/class/block/loop{n}/dev")
                    if num:
                        major, minor = (int(x) for x in num.split(":"))
                        os.mknod(dev, 0o600 | stat.S_IFBLK, os.makedev(major, minor))
                backing = os.open(self.image, os.O_RDWR)
                fd = os.open(dev, os.O_RDWR)
                try:
                    fcntl.ioctl(fd, LOOP_SET_FD, backing)
                except OSError as e:
                    os.close(fd)
                    if e.errno == errno.EBUSY:
                        continue  # someone else took it; try the next free one
                    raise DiskError(f"can't attach {self.image} to {dev}: {e.strerror}") from None
                finally:
                    os.close(backing)
                info = bytearray(232)  # struct loop_info64
                struct.pack_into("I", info, 52, LO_FLAGS_PARTSCAN | LO_FLAGS_AUTOCLEAR)
                name = os.path.abspath(self.image).encode()[:63]
                info[56:56 + len(name)] = name
                try:
                    fcntl.ioctl(fd, LOOP_SET_STATUS64, bytes(info))
                except OSError as e:
                    fcntl.ioctl(fd, LOOP_CLR_FD)
                    os.close(fd)
                    raise DiskError(f"can't set up {dev}: {e.strerror}") from None
                self.dev, self._fd = dev, fd
                return dev
            raise DiskError("no free loop device")
        finally:
            os.close(ctl)

    def __exit__(self, *exc):
        if self._fd is not None:
            try:
                fcntl.ioctl(self._fd, LOOP_CLR_FD)
            except OSError:
                pass  # autoclear detaches it when the last user closes it
            os.close(self._fd)
            self._fd = None


# ---- MBR ----------------------------------------------------------------------

def _chs(lba, heads=255, spt=63):
    """CHS bytes for an LBA, or the 'use LBA' maximum beyond cylinder 1023."""
    c, rem = divmod(lba, heads * spt)
    if c > 1023:
        return bytes([0xFE, 0xFF, 0xFF])
    h, s = divmod(rem, spt)
    return bytes([h, ((s + 1) & 0x3F) | ((c >> 2) & 0xC0), c & 0xFF])


def mbr(partitions, signature, boot_code=b""):
    """A 512-byte MBR. `partitions`: [(start_lba, sectors, type, bootable)],
    at most four. `boot_code`: up to 440 bytes (GRUB's boot.img)."""
    if len(partitions) > 4:
        raise DiskError("an MBR holds at most 4 primary partitions")
    sector = bytearray(SECTOR)
    code = boot_code[:440]
    sector[:len(code)] = code
    struct.pack_into("<I", sector, 440, signature)
    for i, (start, count, ptype, boot) in enumerate(partitions):
        if start + count > 0xFFFFFFFF:
            raise DiskError("disk too large for an MBR (2 TiB)")
        entry = bytes([0x80 if boot else 0]) + _chs(start) + bytes([ptype]) + _chs(start + count - 1) + struct.pack("<II", start, count)
        sector[446 + 16 * i:462 + 16 * i] = entry
    sector[510:512] = b"\x55\xaa"
    return bytes(sector)


def read_mbr(data):
    """(signature, [(start, sectors, type, bootable)]) from an MBR sector."""
    if data[510:512] != b"\x55\xaa":
        raise DiskError("no MBR signature")
    sig = struct.unpack_from("<I", data, 440)[0]
    parts = []
    for i in range(4):
        e = data[446 + 16 * i:462 + 16 * i]
        start, count = struct.unpack_from("<II", e, 8)
        if e[4] and count:
            parts.append((start, count, e[4], e[0] == 0x80))
    return sig, parts


def wipe(fd, size):
    """Zeroes the first and last MiB, so no old partition table (an MBR, or
    a GPT and its backup at the end) or boot code survives."""
    zeros = b"\0" * (1 << 20)
    os.pwrite(fd, zeros[:min(size, len(zeros))], 0)
    if size > 2 * len(zeros):
        os.pwrite(fd, zeros, size - len(zeros))
