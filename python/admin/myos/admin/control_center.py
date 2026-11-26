"""System dashboard for the MyOS desktop terminal."""

import curses
import datetime
import subprocess
import textwrap
import time
from pathlib import Path
from tempfile import TemporaryDirectory
from concurrent.futures import ThreadPoolExecutor

from myos.admin import ai_monitor, control_actions, initctl
from myos.admin.myctl import format_entry
from myos.bus import BusError, Client
from myos.log.v1 import log_pb2
from myos.net.v1 import net_pb2
from myos.sysinfo.v1 import sysinfo_pb2

TABS = ("Overview", "Services", "Apps", "Network", "Logs", "Monitoring")
REFRESH_SECONDS = 2


def overview():
    with Client(timeout=1) as bus:
        system = bus.call("sysinfo", "System", sysinfo_pb2.SystemRequest(),
                          sysinfo_pb2.SystemReply)
        cpu = bus.call("sysinfo", "Cpu", sysinfo_pb2.CpuRequest(), sysinfo_pb2.CpuReply)
        memory = bus.call("sysinfo", "Memory", sysinfo_pb2.MemoryRequest(),
                          sysinfo_pb2.MemoryReply)
    usage = f"{cpu.usage_percent:.1f}%" if cpu.sample_ms else "Waiting for first sample"
    uptime = datetime.timedelta(seconds=int(system.uptime_seconds))
    return [
        f"Host        {system.hostname}",
        f"System      {system.os_name} · {system.machine}",
        f"Kernel      {system.kernel_release}",
        f"Uptime      {uptime}",
        f"CPU         {usage} · {cpu.count} logical CPUs",
        f"Memory      {memory.used_kb / 1024:.0f} / {memory.total_kb / 1024:.0f} MiB",
        f"Load        {system.load1:.2f}  {system.load5:.2f}  {system.load15:.2f}",
        f"Processes   {system.processes}",
    ]


def apps():
    output = control_actions.jrun("list", timeout=5)
    if output.startswith("no apps installed"):
        return ["No apps installed."], []
    rows = output.splitlines()[1:]
    return rows, [row.split()[0] for row in rows]


def network():
    with Client(timeout=1) as bus:
        reply = bus.call("net", "ListInterfaces", net_pb2.ListInterfacesRequest(),
                         net_pb2.ListInterfacesReply)
    rows = ["Network configured" if reply.online else "Network not configured"]
    for interface in reply.interfaces:
        addresses = ", ".join(f"{a.address}/{a.prefix_len}" for a in interface.addresses)
        rows.extend([
            "",
            f"{interface.name}   {interface.method}   {interface.state}",
            f"  IPv4        {addresses or 'No address'}",
            f"  Link        {'Up' if interface.carrier else 'Down'} · MTU {interface.mtu}",
            f"  Gateway     {interface.gateway or '—'}",
            f"  DNS         {', '.join(interface.dns) or '—'}",
        ])
    return rows


def logs(tag, **filters):
    with Client(timeout=1) as bus:
        reply = bus.call("log", "Query", log_pb2.QueryRequest(tag=tag, **filters),
                         log_pb2.QueryReply)
    return [format_entry(entry) for entry in reversed(reply.entries)]


def metric(value, unit="", scale=1):
    return f"{value / scale:.1f}{unit}" if value is not None else "Unavailable"


def monitoring():
    try:
        state = ai_monitor.read_state()
    except FileNotFoundError:
        return ["Waiting for telemetry. Check ai-monitor on the Services tab."]
    samples = state["samples"]
    sample = samples[-1]
    mib = 1024 * 1024
    rows = [
        f"Last collected  {sample['timestamp']}",
        f"CPU             {metric(sample['cpu_percent'], '%')}",
        f"RAM             {metric(sample['memory_used_bytes'], ' MiB', mib)} / "
        f"{metric(sample['memory_total_bytes'], ' MiB', mib)}",
        f"Swap            {metric(sample['swap_used_bytes'], ' MiB', mib)} / "
        f"{metric(sample['swap_total_bytes'], ' MiB', mib)}",
        f"Processes       {sample['processes']} processes · {sample['threads']} threads",
        f"Uptime          {metric(sample['uptime_seconds'], ' s')}",
    ]
    root = sample["root_filesystem"] or {}
    rows.append(f"Root volume     {metric(root.get('available_bytes'), ' MiB', mib)} available / "
                f"{metric(root.get('total_bytes'), ' MiB', mib)} total")
    for label, key in (("Disk read", "disk_read"), ("Disk write", "disk_write"),
                       ("Network RX", "network_rx"), ("Network TX", "network_tx")):
        rows.append(f"{label:<16}{metric(sample[key + '_bytes_per_second'], ' KiB/s', 1024)}")
    temperatures = sample["temperature_celsius"]
    rows.extend(["", "Temperatures"])
    rows.extend([f"{sensor}  {value:.1f} °C" for sensor, value in temperatures.items()]
                or ["No readable thermal zones."])
    rows.extend(["", "Recent system errors"])
    try:
        rows.extend(logs("", limit=10, max_priority=3) or ["No recent error entries."])
    except (OSError, BusError) as error:
        rows.append(f"System logs unavailable: {error}")
    rows.extend(f"{name}: {error}" for name, error in sample["errors"].items())
    rows.extend(["", f"Recent samples ({len(samples)} retained; newest 12 below)",
                 "UTC                       CPU %      RAM %     Swap %"])
    for item in reversed(samples[-12:]):
        values = (metric(item[key]) if item[key] is not None else "—"
                  for key in ("cpu_percent", "memory_percent", "swap_percent"))
        rows.append(f"{item['timestamp'][:23]:<24}" + "".join(f"{value:>10}" for value in values))
    return rows


def snapshot(tab, tag):
    try:
        if tab == "Services":
            services = initctl.status()
            return [f"{service.name:<24} {service.state:<10} {service.detail}"
                    for service in services], [service.name for service in services]
        if tab == "Apps":
            return apps()
        if tab == "Logs":
            return logs(tag, limit=100), []
        return {"Overview": overview, "Network": network, "Monitoring": monitoring}[tab](), []
    except (ValueError, BusError, initctl.InitError, OSError, subprocess.SubprocessError) as error:
        source = {"Overview": "System information", "Services": "Service status",
                  "Apps": "App inventory", "Network": "Network status", "Logs": "Logs", "Monitoring": "Telemetry"}[tab]
        detail = getattr(error, "stderr", "") or str(error)
        return [f"{source} unavailable", detail.strip()], []


def put(screen, y, line, style=0):
    height, width = screen.getmaxyx()
    if 0 <= y < height and width > 1:
        # Log messages may contain controls. Keep them on their assigned row.
        safe = "".join(char if char.isprintable() else " " for char in line)
        try:
            screen.addnstr(y, 0, safe, width - 1, style)
        except curses.error:
            # Wide characters can still reach the right edge.
            pass


def package_path(screen):
    value = ""
    while True:
        screen.erase()
        put(screen, 0, "Install or upgrade a local app", curses.A_BOLD)
        put(screen, 1, "Type or paste the absolute guest .mapp path.")
        put(screen, 2, "Enter Review package   Esc Cancel")
        _, width = screen.getmaxyx()
        visible = max(0, width - 3)
        put(screen, 4, "> " + (value[-visible:] if visible else ""))
        key = screen.get_wch()
        if key == "\x1b":
            return ""
        if key in ("\n", "\r", curses.KEY_ENTER):
            return value
        if key in ("\b", "\x7f", curses.KEY_BACKSPACE):
            value = value[:-1]
        elif isinstance(key, str) and key.isprintable() and len(value) < 4096:
            value += key


def dialog(screen, title, lines, confirm=False):
    """Wrap long output and keep every requested permission readable."""
    offset = 0
    while True:
        screen.erase()
        height, width = screen.getmaxyx()
        page = max(1, height - 3)
        wrapped = [part for line in lines for part in
                   (textwrap.wrap(line, max(1, width - 1)) or [""])]
        offset = max(0, min(offset, len(wrapped) - page))
        put(screen, 0, title, curses.A_BOLD)
        for y, line in enumerate(wrapped[offset:offset + page], 1):
            put(screen, y, line)
        footer = "Y Confirm   Esc Cancel" if confirm else "Enter or Esc Close"
        put(screen, height - 1, footer + "   ↑↓ Scroll")
        key = screen.getch()
        if confirm and key in (ord("y"), ord("Y")):
            return True
        if key == 27 or (not confirm and key in (10, 13, curses.KEY_ENTER)):
            return False
        offset += {curses.KEY_UP: -1, curses.KEY_DOWN: 1,
                   curses.KEY_PPAGE: -page, curses.KEY_NPAGE: page}.get(key, 0)


def dashboard(screen, executor, staged):
    screen.timeout(150)
    try:
        curses.curs_set(0)
    except curses.error:
        pass
    tab, tag = "Overview", ""
    rows, targets = ["Loading…"], []
    position = offset = 0
    generation = requested_generation = 0
    refresh_at = 0.0
    pending = None
    operation = None
    operation_name = ""

    while True:
        if operation is not None and operation.done():
            try:
                message = operation.result()
            except (OSError, ValueError, initctl.InitError, subprocess.SubprocessError) as error:
                dialog(screen, f"{operation_name} failed", str(error).splitlines())
            else:
                if operation_name == "Review package":
                    if dialog(screen, "Install or upgrade this app?",
                              message.splitlines(), confirm=True):
                        operation_name = "Install app"
                        operation = executor.submit(control_actions.apply, "install", staged)
                        continue
                else:
                    dialog(screen, f"{operation_name} response",
                           message.splitlines() or ["Request completed."])
            operation = None
            staged.unlink(missing_ok=True)
            generation += 1
            refresh_at = 0

        if pending is not None and pending.done():
            new_rows, new_targets = pending.result()
            pending = None
            if requested_generation == generation:
                if new_targets and position < len(targets):
                    selected = targets[position]
                    if selected in new_targets:
                        position = new_targets.index(selected)
                rows, targets = new_rows, new_targets
                refresh_at = time.monotonic() + REFRESH_SECONDS
        if pending is None and operation is None and time.monotonic() >= refresh_at:
            requested_generation = generation
            pending = executor.submit(snapshot, tab, tag)

        screen.erase()
        height, _ = screen.getmaxyx()
        page = max(1, height - 6)
        position = max(0, min(position, len(rows) - 1))
        offset = min(offset, position)
        offset = max(offset, position - page + 1)
        offset = min(offset, max(0, len(rows) - page))
        put(screen, 0, "MyOS Control Center", curses.A_BOLD)
        put(screen, 1, "   ".join(f"{i} {name}" if name != tab else f"[{i} {name}]"
                                  for i, name in enumerate(TABS, 1)))
        if tab == "Logs":
            put(screen, 2, f"{tag or 'All services'} · Newest first · UTC")
        elif tab == "Services":
            put(screen, 2, f"{'Service':<24} {'State':<10} Detail")
        elif tab == "Apps":
            put(screen, 2, f"{'ID':<32} {'VERSION':<10} {'USER':<16} STATE")
        if not rows:
            put(screen, 3, "No matching logs." if tab == "Logs" else "No services.")
        for index, row in enumerate(rows[offset:offset + page], offset):
            style = curses.A_REVERSE if index == position and targets else 0
            put(screen, index - offset + 3, row, style)
        help_text = "1–6 Tabs   ↑↓ Scroll   R Refresh   Q Quit"
        if tab == "Services":
            put(screen, height - 2, "Enter View logs   S Start   T Stop   X Restart")
        elif tab == "Apps":
            put(screen, height - 2, "I Install or upgrade   D Remove and keep data")
        elif tab == "Logs" and tag:
            help_text += "   A All logs"
        if operation is not None:
            put(screen, height - 1, f"{operation_name} in progress…")
        else:
            put(screen, height - 1, help_text)
        screen.refresh()

        key = screen.getch()
        if operation is not None:
            continue
        letter = chr(key).lower() if 0 <= key < 128 else ""

        if tab == "Services" and targets and letter in control_actions.SERVICE_ACTIONS:
            action = control_actions.SERVICE_ACTIONS[letter]
            target = targets[position]
            if dialog(screen, f"{action.title()} {target}?",
                      ["This closes the graphical session."] if target == "desktop"
                      and action != "start" else [], confirm=True):
                operation_name = f"{action.title()} {target}"
                operation = executor.submit(control_actions.apply, action, target)
            continue
        if tab == "Apps" and letter == "d" and targets:
            target = targets[position]
            if dialog(screen, f"Remove {target}?", ["App data will be kept for reinstall."],
                      confirm=True):
                operation_name = f"Remove {target}"
                operation = executor.submit(control_actions.apply, "remove", target)
            continue
        if tab == "Apps" and letter == "i":
            screen.timeout(-1)
            try:
                path = package_path(screen)
            finally:
                screen.timeout(150)
            if path:
                operation_name = "Review package"
                operation = executor.submit(control_actions.preview, path, staged)
            continue

        if key in (ord("q"), ord("Q")):
            return
        if 0 <= key - ord("1") < len(TABS):
            tab, tag = TABS[key - ord("1")], ""
        elif key in (curses.KEY_ENTER, 10, 13) and tab == "Services" and targets:
            tab, tag = "Logs", targets[position]
        elif key in (ord("a"), ord("A")) and tab == "Logs":
            tag = ""
        elif key in (ord("r"), ord("R")):
            generation += 1
            refresh_at = 0
            continue
        else:
            position += {curses.KEY_UP: -1, curses.KEY_DOWN: 1,
                         curses.KEY_PPAGE: -page, curses.KEY_NPAGE: page}.get(key, 0)
            continue

        generation += 1
        position = offset = refresh_at = 0
        rows, targets = ["Loading…"], []


def main():
    with TemporaryDirectory(prefix="myos-control-center-") as directory:
        with ThreadPoolExecutor(max_workers=1) as executor:
            curses.wrapper(dashboard, executor, Path(directory) / "package.mapp")
