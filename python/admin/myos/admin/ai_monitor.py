"""Managed telemetry collection and rolling baselines for MyOS."""

import argparse
from collections import deque
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import stat
from statistics import fmean, pstdev
import time
import tomllib

CONFIG = Path("/etc/myos/ai-monitor.toml")
STATE = Path("/run/myos/ai-monitor")
BASELINE_FEATURES = (
    "cpu_percent", "memory_percent", "swap_percent", "disk_read_bytes_per_second",
    "disk_write_bytes_per_second", "network_rx_bytes_per_second",
    "network_tx_bytes_per_second",
)


def settings(path):
    with path.open("rb") as stream:
        config = tomllib.load(stream)
    interval = config.get("interval_seconds", 5)
    capacity = config.get("history_samples", 120)
    if type(interval) not in (float, int) or not 0.25 <= interval <= 300:
        raise ValueError("interval_seconds must be between 0.25 and 300")
    if type(capacity) is not int or not 2 <= capacity <= 1000:
        raise ValueError("history_samples must be an integer between 2 and 1000")
    return interval, capacity


def structured_log(event, **fields):
    print(json.dumps({"event": event, **fields}), flush=True)


def baselines(samples):
    result = {}
    for feature in BASELINE_FEATURES:
        values = [sample[feature] for sample in samples if sample.get(feature) is not None]
        if values:
            result[feature] = {
                "count": len(values), "mean": fmean(values), "stddev": pstdev(values),
            }
    return result


def publish(directory, samples):
    # Publish one complete snapshot so CLI and dashboard readers can race the writer.
    temporary = directory / "metrics.tmp"
    try:
        with temporary.open("w") as stream:
            json.dump({"schema_version": 1, "samples": list(samples),
                       "baseline": baselines(samples)}, stream, allow_nan=False)
        temporary.replace(directory / "metrics.json")
    finally:
        temporary.unlink(missing_ok=True)


def read_state(directory=STATE):
    return json.loads((directory / "metrics.json").read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=CONFIG)
    parser.add_argument("--state-dir", type=Path, default=STATE)
    parser.add_argument("--once", action="store_true", help="collect one sample and exit")
    parser.add_argument("--report", action="store_true", help="print recent samples and baselines")
    args = parser.parse_args()
    try:
        if args.report:
            print(json.dumps(read_state(args.state_dir), indent=2))
            return 0
        interval, capacity = settings(args.config)
        args.state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = args.state_dir.lstat()
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid()
                or info.st_mode & 0o077):
            raise ValueError("state directory must be private and owned by the service user")
        with (args.state_dir / "writer.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return collect(args.state_dir, interval, capacity, args.once)
    except (OSError, ValueError) as error:
        structured_log("monitor_failed", error=str(error))
        return 1


def collect(directory, interval, capacity, once):
    from myos.admin.telemetry import Telemetry

    telemetry = Telemetry()
    try:
        history = deque(read_state(directory)["samples"], maxlen=capacity)
    except (FileNotFoundError, ValueError, KeyError, TypeError):
        history = deque(maxlen=capacity)
    structured_log("monitor_started", interval_seconds=interval, history_samples=capacity)
    previous_errors = {}
    while True:
        sample = telemetry.sample()
        sample["timestamp"] = datetime.now(timezone.utc).isoformat(timespec="milliseconds")
        if sample["errors"] != previous_errors:
            structured_log("telemetry_sources_changed", errors=sample["errors"])
            previous_errors = sample["errors"]
        history.append(sample)
        publish(directory, history)
        if once:
            return 0
        time.sleep(interval)
