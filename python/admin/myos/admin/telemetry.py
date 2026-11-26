"""Read Linux resource counters and turn consecutive samples into rates."""

import os
from pathlib import Path
import time


def counters(previous, current, elapsed):
    if previous is None or elapsed <= 0 or previous.keys() != current.keys() or not current:
        return None
    deltas = [current[name] - previous[name] for name in current]
    return round(sum(deltas) / elapsed, 2) if min(deltas) >= 0 else None


class Telemetry:
    def __init__(self, proc=Path("/proc"), sys=Path("/sys"), root=Path("/")):
        self.proc = proc
        self.sys = sys
        self.root = root
        self.previous = None

    def read(self, source, reader, errors):
        try:
            return reader()
        except (OSError, ValueError, IndexError, KeyError) as error:
            errors[source] = str(error)
            return None

    def cpu(self):
        fields = (self.proc / "stat").read_text().splitlines()[0].split()[1:9]
        ticks = [int(value) for value in fields]
        return sum(ticks), sum(ticks[3:5])

    def memory(self):
        return {line.split(":")[0]: int(line.split(":")[1].split()[0])
                for line in (self.proc / "meminfo").read_text().splitlines()}

    def disk(self):
        devices = {path.name for path in (self.sys / "block").iterdir()
                   if not path.name.startswith(("loop", "ram", "zram"))
                   and not any((path / "slaves").iterdir())}
        rows = [line.split() for line in (self.proc / "diskstats").read_text().splitlines()]
        return tuple({row[2]: int(row[column]) * 512 for row in rows
                      if row[2] in devices} for column in (5, 9))

    def network(self):
        lines = (self.proc / "net/dev").read_text().splitlines()[2:]
        rows = {name.strip(): data.split() for name, data in
                (line.split(":", 1) for line in lines)}
        rows.pop("lo", None)
        return tuple({name: int(row[column]) for name, row in rows.items()}
                     for column in (0, 8))

    def usage(self):
        data = os.statvfs(self.root)
        return {"total_bytes": data.f_blocks * data.f_frsize,
                "used_bytes": (data.f_blocks - data.f_bfree) * data.f_frsize,
                "available_bytes": data.f_bavail * data.f_frsize}

    def temperatures(self):
        sensors = {}
        for path in sorted((self.sys / "class/thermal").glob("thermal_zone*")):
            try:
                name = (path / "type").read_text().strip()
                sensors[f"{path.name}:{name}"] = int((path / "temp").read_text()) / 1000
            except (OSError, ValueError):
                continue
        return sensors

    def sample(self):
        now = time.monotonic()
        previous_time, old_cpu, old_disk, old_net = self.previous or (now, None, None, None)
        errors = {}
        cpu = self.read("cpu", self.cpu, errors)
        memory = self.read("memory", self.memory, errors)
        disk = self.read("disk_io", self.disk, errors)
        network = self.read("network", self.network, errors)
        load = self.read("load", lambda: (self.proc / "loadavg").read_text().split(), errors)
        processes = self.read("processes", lambda: sum(p.name.isdecimal()
                              for p in self.proc.iterdir()), errors)
        values = {
            "uptime_seconds": self.read("uptime", lambda: float(
                (self.proc / "uptime").read_text().split()[0]), errors),
            "processes": processes,
            # The denominator of /proc/loadavg's runnable/total field counts tasks.
            "threads": self.read("threads", lambda: int(load[3].split("/")[1]), errors)
            if load else None,
            "root_filesystem": self.read("root_filesystem", self.usage, errors),
            "temperature_celsius": self.temperatures(),
            "errors": errors,
            "cpu_percent": None,
        }
        for feature, total_key, free_key in (
            ("memory", "MemTotal", "MemAvailable"), ("swap", "SwapTotal", "SwapFree")
        ):
            total = memory.get(total_key) if memory else None
            free = memory.get(free_key) if memory else None
            used = max(0, total - free) if total is not None and free is not None else None
            values[f"{feature}_total_bytes"] = total * 1024 if total is not None else None
            values[f"{feature}_used_bytes"] = used * 1024 if used is not None else None
            values[f"{feature}_percent"] = round(100 * used / total, 2) if total and used is not None else None
        if old_cpu is not None and cpu is not None:
            total, idle = cpu[0] - old_cpu[0], cpu[1] - old_cpu[1]
            if total > 0 and 0 <= idle <= total:
                values["cpu_percent"] = round(100 * (total - idle) / total, 2)
        for keys, before, current in (
            (("disk_read", "disk_write"), old_disk, disk),
            (("network_rx", "network_tx"), old_net, network),
        ):
            for index, key in enumerate(keys):
                rate = counters(before[index], current[index], now - previous_time) if before and current else None
                values[f"{key}_bytes_per_second"] = rate
        self.previous = now, cpu, disk, network
        return values
