"""Copy a directory tree to another filesystem, like `cp -ax`.

Keeps owners, permissions (setuid included), timestamps, extended
attributes (file capabilities, ACLs), symlinks, hard links, device nodes
and FIFOs. Stays on one filesystem: a directory that is a mount point
(/proc, /dev, /run, ...) is created empty and not entered.
"""

import os
import stat


class CopyError(Exception):
    pass


class Copier:
    """copy(src, dst): `empty` are paths (relative to src, e.g. "tmp") that
    are created but not filled; `skip` are left out altogether. `progress`
    is called with (bytes_done, bytes_total) now and then."""

    def __init__(self, empty=(), skip=(), progress=None):
        self.empty = {p.strip("/") for p in empty}
        self.skip = {p.strip("/") for p in skip}
        self.progress = progress
        self.done = 0
        self.total = 1
        self.files = 0
        self._links = {}
        self._dirs = []
        self._next_report = 0

    def measure(self, src):
        """Bytes of file data copy(src, ...) would copy."""
        src = os.path.abspath(src)
        dev = os.lstat(src).st_dev
        total = 0
        seen = set()
        stack = [""]
        while stack:
            rel = stack.pop()
            try:
                entries = list(os.scandir(os.path.join(src, rel) if rel else src))
            except OSError:
                continue
            for e in entries:
                r = f"{rel}/{e.name}" if rel else e.name
                if r in self.skip:
                    continue
                st = e.stat(follow_symlinks=False)
                if stat.S_ISDIR(st.st_mode):
                    if st.st_dev == dev and r not in self.empty:
                        stack.append(r)
                elif stat.S_ISREG(st.st_mode) and (st.st_dev, st.st_ino) not in seen:
                    if st.st_nlink > 1:
                        seen.add((st.st_dev, st.st_ino))
                    total += st.st_size
        return total

    def copy(self, src, dst, total=None):
        src = os.path.abspath(src)
        top = os.lstat(src)
        self.total = max(1, total if total is not None else self.measure(src))
        self._dirs = []
        self._walk(src, dst, "", top.st_dev)
        # Directory times last: creating their entries changed them.
        for path, st in reversed(self._dirs):
            self._meta(path, st)
        self._report(force=True)

    def _walk(self, src, dst, rel, dev):
        try:
            entries = sorted(os.scandir(os.path.join(src, rel) if rel else src), key=lambda e: e.name)
        except OSError as e:
            raise CopyError(f"can't read /{rel}: {e.strerror}") from None
        for e in entries:
            r = f"{rel}/{e.name}" if rel else e.name
            if r in self.skip:
                continue
            s = os.path.join(src, r)
            d = os.path.join(dst, r)
            st = os.lstat(s)
            mode = st.st_mode
            if stat.S_ISDIR(mode):
                try:
                    os.mkdir(d, 0o700)
                except FileExistsError:
                    pass  # lost+found, made by mkfs
                self._dirs.append((d, st))
                if st.st_dev == dev and r not in self.empty:
                    self._walk(src, dst, r, dev)
                continue
            if stat.S_ISSOCK(mode):
                continue  # meaningless once copied
            key = (st.st_dev, st.st_ino)
            if st.st_nlink > 1 and key in self._links:
                os.link(self._links[key], d)
                continue
            if stat.S_ISREG(mode):
                self._file(s, d, st)
            elif stat.S_ISLNK(mode):
                os.symlink(os.readlink(s), d)
            elif stat.S_ISCHR(mode) or stat.S_ISBLK(mode) or stat.S_ISFIFO(mode):
                os.mknod(d, mode, st.st_rdev)
            else:
                continue
            self._meta(d, st, src=s)
            if st.st_nlink > 1:
                self._links[key] = d
            self.files += 1

    def _file(self, s, d, st):
        try:
            fin = os.open(s, os.O_RDONLY | os.O_NOFOLLOW)
        except OSError as e:
            raise CopyError(f"can't read {s}: {e.strerror}") from None
        try:
            fout = os.open(d, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                while True:
                    n = os.sendfile(fout, fin, None, 8 << 20)
                    if n == 0:
                        break
                    self.done += n
                    self._report()
            except OSError as e:
                raise CopyError(f"can't write {d}: {e.strerror}") from None
            finally:
                os.close(fout)
        finally:
            os.close(fin)

    def _meta(self, d, st, src=None):
        link = stat.S_ISLNK(st.st_mode)
        try:
            os.chown(d, st.st_uid, st.st_gid, follow_symlinks=False)
        except PermissionError:
            pass  # not root (tests): keep our own ownership
        if not link:
            os.chmod(d, stat.S_IMODE(st.st_mode))  # after chown, which clears setuid
        if src is not None:
            try:
                for name in os.listxattr(src, follow_symlinks=False):
                    os.setxattr(d, name, os.getxattr(src, name, follow_symlinks=False), follow_symlinks=False)
            except OSError:
                pass  # no xattr support on one side
        os.utime(d, ns=(st.st_atime_ns, st.st_mtime_ns), follow_symlinks=False)

    def _report(self, force=False):
        if self.progress and (force or self.done >= self._next_report):
            self.progress(self.done, self.total)
            self._next_report = self.done + self.total // 20
