"""Edit TOML files line by line, so comments and layout survive.

Covers the subset myos-init and netd read (rust/init/src/toml.rs): top-level
keys, [tables], strings, integers, booleans and arrays, which may span lines.
Reading is done with the standard library's tomllib; this module only
writes. Every edit is checked by parsing the result before it is returned.

    text = set_value(text, "eth0", "method", "static")
    text = set_value(text, None, "enabled", False)       # top-level key
    text = delete_key(text, "eth0", "gateway")
    text = delete_table(text, "eth1")
"""

import os
import re
import tempfile
import tomllib

_HEADER = re.compile(r"^\s*\[\s*([A-Za-z0-9_.-]+)\s*\]\s*(#.*)?$")
_KEY = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")
_BARE_KEY = re.compile(r"^[A-Za-z0-9_-]+$")


class EditError(Exception):
    pass


def render(value):
    """A Python value as a TOML literal of the supported subset."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str):
        out = ['"']
        for ch in value:
            if ch == '"':
                out.append('\\"')
            elif ch == "\\":
                out.append("\\\\")
            elif ch == "\n":
                out.append("\\n")
            elif ch == "\t":
                out.append("\\t")
            elif ch == "\r":
                out.append("\\r")
            elif ord(ch) < 0x20 or ord(ch) == 0x7F:
                out.append(f"\\u{ord(ch):04X}")
            else:
                out.append(ch)
        out.append('"')
        return "".join(out)
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(render(v) for v in value) + "]"
    raise EditError(f"can't write a {type(value).__name__} to TOML")


def _depth(line):
    """How many [ ] brackets `line` leaves open, ignoring strings and comments."""
    depth = 0
    quote = None
    i = 0
    while i < len(line):
        ch = line[i]
        if quote:
            if ch == "\\" and quote == '"':
                i += 1
            elif ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch == "#":
            break
        elif ch == "[":
            depth += 1
        elif ch == "]":
            depth -= 1
        i += 1
    return depth


def _layout(lines):
    """Yields (kind, table, key, start, end) for each item: kind is "header"
    or "key"; end is exclusive and covers a value continued on later lines."""
    table = None
    i = 0
    while i < len(lines):
        line = lines[i]
        m = _HEADER.match(line)
        if m:
            table = m.group(1)
            yield "header", table, None, i, i + 1
            i += 1
            continue
        m = _KEY.match(line)
        if m:
            start = i
            depth = _depth(line.split("=", 1)[1])
            i += 1
            while depth > 0 and i < len(lines):
                depth += _depth(lines[i])
                i += 1
            yield "key", table, m.group(1), start, i
            continue
        i += 1


def _check(text):
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise EditError(f"edit produced invalid TOML: {e}") from e


def set_value(text, table, key, value):
    """Sets `key` in [table] (None: top level), adding the key, or the table
    at the end of the file, as needed."""
    if not _BARE_KEY.match(key) or (table is not None and not _BARE_KEY.match(table)):
        raise EditError(f"unsupported key name: {table + '.' if table else ''}{key}")
    lines = text.splitlines()
    new = f"{key} = {render(value)}"
    items = list(_layout(lines))
    for kind, t, k, start, end in items:
        if kind == "key" and t == table and k == key:
            lines[start:end] = [new]
            return _finish(lines)
    # Not there yet: after the table's last key, else right after its header.
    if table is None:
        tops = [it for it in items if it[1] is None and it[0] == "key"]
        first_header = next((it[3] for it in items if it[0] == "header"), None)
        if tops:
            at = tops[-1][4]
        elif first_header is not None:
            at = first_header
            # Keep a blank line between the new key and the first table.
            lines[at:at] = [new, ""]
            return _finish(lines)
        else:
            at = len(lines)
        lines[at:at] = [new]
        return _finish(lines)
    in_table = [it for it in items if it[1] == table]
    if not in_table:
        while lines and not lines[-1].strip():
            lines.pop()
        if lines:
            lines.append("")
        lines += [f"[{table}]", new]
        return _finish(lines)
    at = in_table[-1][4]
    lines[at:at] = [new]
    return _finish(lines)


def delete_key(text, table, key):
    """Removes `key` from [table]; no change if it isn't there."""
    lines = text.splitlines()
    for kind, t, k, start, end in _layout(lines):
        if kind == "key" and t == table and k == key:
            del lines[start:end]
            return _finish(lines)
    return text


def delete_table(text, table):
    """Removes [table] with its keys and the comments inside it."""
    lines = text.splitlines()
    items = list(_layout(lines))
    headers = [it for it in items if it[0] == "header"]
    for n, (_, t, _, start, _) in enumerate(headers):
        if t == table:
            end = headers[n + 1][3] if n + 1 < len(headers) else len(lines)
            del lines[start:end]
            # Drop the blank line that separated it from what came before.
            while start > 0 and not lines[start - 1].strip() and (start == len(lines) or not lines[start].strip()):
                del lines[start - 1]
                start -= 1
            return _finish(lines)
    return text


def _finish(lines):
    while lines and not lines[-1].strip():
        lines.pop()
    text = "\n".join(lines) + "\n" if lines else ""
    _check(text)
    return text


def write_atomic(path, text, mode=0o644):
    """Replaces `path` with `text` without ever leaving a half-written file."""
    d = os.path.dirname(path) or "."
    try:
        mode = os.stat(path).st_mode & 0o7777
    except FileNotFoundError:
        pass
    fd, tmp = tempfile.mkstemp(dir=d, prefix="." + os.path.basename(path) + ".")
    try:
        with os.fdopen(fd, "w") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise
