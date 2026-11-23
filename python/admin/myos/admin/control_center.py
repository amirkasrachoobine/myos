"""Read-only dashboard for the MyOS desktop terminal."""

import curses
import datetime
import shutil
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor

from myos.admin import initctl
from myos.admin.myctl import format_entry
from myos.bus import BusError, Client
from myos.log.v1 import log_pb2
from myos.net.v1 import net_pb2
from myos.sysinfo.v1 import sysinfo_pb2

TABS = ("Overview", "Services", "Apps", "Network", "Logs")
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
    jrun = shutil.which("jrun")
    if not jrun:
        return ["App inventory unavailable. jrun is missing."]
    result = subprocess.run([jrun, "list"], check=True, capture_output=True,
                            text=True, errors="replace", timeout=2)
    if result.stdout.startswith("no apps installed"):
        return ["No apps installed."]
    return result.stdout.splitlines()


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


def logs(tag):
    with Client(timeout=1) as bus:
        reply = bus.call("log", "Query", log_pb2.QueryRequest(limit=100, tag=tag),
                         log_pb2.QueryReply)
    return [format_entry(entry) for entry in reversed(reply.entries)]


def snapshot(tab, tag):
    try:
        if tab == "Services":
            services = initctl.status()
            return [f"{service.name:<24} {service.state:<10} {service.detail}"
                    for service in services], [service.name for service in services]
        if tab == "Logs":
            return logs(tag), []
        return {"Overview": overview, "Apps": apps, "Network": network}[tab](), []
    except (BusError, initctl.InitError, OSError, subprocess.SubprocessError) as error:
        source = {"Overview": "System information", "Services": "Service status",
                  "Apps": "App inventory", "Network": "Network status", "Logs": "Logs"}[tab]
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


def dashboard(screen, executor):
    screen.timeout(150)
    try:
        curses.curs_set(0)
    except curses.error:
        pass
    tab, tag = "Overview", ""
    rows, services = ["Loading…"], []
    position = offset = 0
    generation = requested_generation = 0
    refresh_at = 0.0
    pending = None

    while True:
        if pending is not None and pending.done():
            new_rows, new_services = pending.result()
            pending = None
            if requested_generation == generation:
                if new_services and position < len(services):
                    selected = services[position]
                    if selected in new_services:
                        position = new_services.index(selected)
                rows, services = new_rows, new_services
                refresh_at = time.monotonic() + REFRESH_SECONDS
        if pending is None and time.monotonic() >= refresh_at:
            requested_generation = generation
            pending = executor.submit(snapshot, tab, tag)

        screen.erase()
        height, _ = screen.getmaxyx()
        page = max(1, height - 5)
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
        if not rows:
            put(screen, 3, "No matching logs." if tab == "Logs" else "No services.")
        for index, row in enumerate(rows[offset:offset + page], offset):
            style = curses.A_REVERSE if index == position and services else 0
            put(screen, index - offset + 3, row, style)
        help_text = "1–5 Tabs   ↑↓ Scroll   R Refresh   Q Quit"
        if tab == "Services":
            help_text += "   Enter View logs"
        elif tab == "Logs" and tag:
            help_text += "   A All logs"
        put(screen, height - 1, help_text)
        screen.refresh()

        key = screen.getch()
        if key in (ord("q"), ord("Q")):
            return
        if ord("1") <= key <= ord("5"):
            tab, tag = TABS[key - ord("1")], ""
        elif key in (curses.KEY_ENTER, 10, 13) and tab == "Services" and services:
            tab, tag = "Logs", services[position]
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
        rows, services = ["Loading…"], []


def main():
    with ThreadPoolExecutor(max_workers=1) as executor:
        curses.wrapper(dashboard, executor)
