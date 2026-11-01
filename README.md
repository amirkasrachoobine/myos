# myos

A headless Linux-based OS with a userland written in Rust, C++, Python and Java.
Phases 0–3 are done: boot, the Rust init, the IPC bus, and the core services.

**Done when:** `make test` boots the image in QEMU, reaches the login prompt, and
every language smoke test passes.

## Quick start

Host: x86_64 Linux (Debian/Ubuntu shown) with ~30 GB free disk.

```sh
sudo apt install build-essential git file wget cpio unzip rsync bc \
                 python3 libncurses-dev libelf-dev libssl-dev
make            # first build: ~1–2 h (toolchain, kernel, QEMU); later builds are minutes
make run        # boots to a serial login prompt — root / myos — exit with Ctrl-A X
make test       # automated boot test
```

Add `WITH_JAVA=1` to `make` to build OpenJDK 21 into the image as well (adds
about an hour). It isn't needed until Phase 6, so it's off by default.

## What's in the image

| Piece | Source | Notes |
|---|---|---|
| Kernel | Linux 6.18.7 | Buildroot's QEMU x86_64 config + `build/board/qemu-x86_64/linux.fragment` |
| libc / toolchain | glibc, GCC with C++ | built by Buildroot (Rust and OpenJDK both need glibc) |
| Init (temporary) | BusyBox init + getty on `ttyS0` | replaced by our Rust PID 1 in Phase 1 |
| Rust | `rust/hello` → `/usr/bin/myos-hello-rust` | built with Buildroot's `cargo-package` |
| C++ | `cpp/hello` → `/usr/bin/myos-hello-cpp` | built with the target `g++` |
| Python | Python 3 + `/usr/bin/myos-hello-py` | |
| Java | OpenJDK 21 | opt-in, `WITH_JAVA=1` |
| QEMU | built on the host by Buildroot | so you don't depend on the distro's version |

## Layout

```
myos/
├── Makefile                 # driver: fetches Buildroot 2026.02.x (LTS) and builds
├── build/                   # Buildroot external tree (BR2_EXTERNAL=build)
│   ├── configs/             # myos_qemu_x86_64_defconfig, java.fragment
│   ├── board/qemu-x86_64/   # kernel fragment, rootfs overlay
│   └── package/             # our Buildroot packages, one per component
├── rust/                    # Rust crates (init, daemons from Phase 1)
├── cpp/                     # C++ shell + utilities (Phase 4)
├── python/                  # admin tools (Phase 5)
├── java/                    # app runtime + SDK (Phase 6)
├── proto/                   # shared IPC definitions (Phase 2)
├── scripts/run-qemu.sh
└── tests/boot_test.py
```

## Day-to-day

- Edit a component, then rebuild just that package: `make rebuild-myos-hello-rust`
- Change Buildroot options: `make menuconfig`, then `make savedefconfig` to write them back to `build/configs/`
- Change kernel options: add them to `linux.fragment` (preferred) or use `make linux-menuconfig`
- Fast iteration on Rust/C++ code: build and test natively on your host first (`cargo run`, `make`), then rebuild the package for the image

## Adding a component

Every later phase follows the same pattern as the hello packages:

1. Put the source under `rust/`, `cpp/`, `python/` or `java/`.
2. Add `build/package/<name>/Config.in` and `<name>.mk` (`cargo-package` for Rust,
   `generic-package` for C++/Java, `python-package` for Python).
3. `source` it from `build/Config.in` and enable it in the defconfig.

Phase 1 is the first real one: `rust/init` becomes PID 1, and the defconfig
switches `BR2_INIT_BUSYBOX` to `BR2_INIT_NONE` with `init=/sbin/myos-init`.

## Phase 1: the Rust init

`rust/init` builds `/usr/bin/myos-init`, which runs as PID 1 (`/sbin/init`
links to it). At boot it mounts `/proc`, `/sys`, `/dev`, `/dev/pts`,
`/dev/shm`, `/run` and `/tmp`, remounts `/` read-write, sets the hostname
from `/etc/hostname`, then starts the services in `/etc/myos/services/`.
It reaps every zombie, restarts crashed services with backoff (1s, 2s, 4s
… 30s), and on `poweroff` / `reboot` / `halt` / Ctrl-Alt-Del it stops the
services, kills stragglers, syncs, remounts `/` read-only and calls the
kernel.

A service is one TOML file; the service name is the file name:

```toml
# /etc/myos/services/syslogd.toml
description = "System log daemon"
exec = ["/sbin/syslogd", "-n"]   # absolute path; no shell, no PATH lookup
type = "simple"                  # or "oneshot": runs once at boot
restart = "always"               # "always" | "on-failure" (default) | "never"
after = ["network"]              # wait for these to be up (oneshots: finished)
enabled = true
console = false                  # true: stdin too, for interactive programs
stop_signal = "TERM"
stop_timeout = 5                 # seconds before SIGKILL

[env]
LANG = "C.UTF-8"
```

Inside the VM:

```sh
initctl status                # every service
initctl status getty
initctl stop|start|restart syslogd
initctl reload                # re-read /etc/myos/services (also: kill -HUP 1)
poweroff                      # BusyBox's poweroff/reboot/halt work as usual
```

On your machine:

```sh
myos-init check build/board/qemu-x86_64/rootfs-overlay/etc/myos/services
make test-init                # init as PID 1 in namespaces, ~15 s, needs sudo
make rebuild-myos-init && make test
```

Add `myos.emergency` to the kernel command line to skip the services and
get a root shell on the console. If init itself ever fails, it drops to a
shell rather than letting the kernel panic.

## Phase 2: the IPC bus

Every component talks to every other through one broker, `busd`, on the
Unix socket `/run/myos/bus.sock`. Messages are protobuf, defined once in
`proto/` and generated for each language at build time.

```
proto/myos/bus/v1/wire.proto   the envelope: Frame, Call, Reply, Register, Status
proto/myos/bus/v1/bus.proto    the broker's own service "bus": Ping, ListServices
rust/bus/                      busd, the myos-bus CLI, and the Rust library (myos_bus)
cpp/bus/                       libmyosbus + <myos/bus.hpp>, myos-bus-ping-cpp
python/bus/                    the myos.bus module, myos-bus-ping-py
java/bus/                      myos-bus.jar (org.myos.bus.BusClient), myos-bus-ping-java
```

**How a call works.** A client sends `Call{service, method, payload}`; the
payload is that method's request message. busd forwards it to whichever
connection registered `service`, stamps it with the caller's uid/gid/pid
from the kernel (SO_PEERCRED), and routes the `Reply` back. Errors come back
as a status code: `UNKNOWN_SERVICE`, `UNKNOWN_METHOD`, `BAD_REQUEST`,
`TIMEOUT` (30 s by default; clients send their own deadline, default 5 s),
`UNAVAILABLE` (the service died mid-call), `PERMISSION_DENIED`, and so on.
Only root may register service names, except names under `app.`, which
Phase 6 apps will use.

Inside the VM:

```sh
myos-bus ping                 # Rust; also myos-bus-ping-cpp, -py, -java
myos-bus list                 # services on the bus
myos-bus-selftest             # every client in the image pings busd
```

On your machine:

```sh
make test-bus                 # build busd + all four clients, ping from each, ~1 min
cd rust/bus && cargo test     # broker tests: routing, timeouts, crashes, concurrency
```

`make test-bus` needs `protoc`, the C++ protobuf library, Python's protobuf
module, `cmake` and `pkg-config`, plus a JDK for the Java part; on
Debian/Ubuntu: `apt install protobuf-compiler libprotobuf-dev python3-protobuf
cmake pkg-config openjdk-21-jdk-headless`. It fetches protobuf-java from Maven
Central into `java/bus/build/`, or skips Java if it can't.

**Writing a service (Phase 3).** Put its messages in `proto/myos/<name>/v1/`,
then in Rust:

```rust
use myos_bus::{reply, Service, Status};
Service::register(&["sysinfo"])?.serve(|req| match req.method {
    "Memory" => {
        let _r: MemoryRequest = req.decode()?;      // BAD_REQUEST if it doesn't decode
        reply(&MemoryReply { total_kb: 0, ..Default::default() })
    }
    m => Err(Status::unknown_method(m)),
})?;
```

and add `after = ["busd"]` to its service file. Clients then call it with
`call("sysinfo", "Memory", request)` in any of the four languages.

**Build notes.** Buildroot builds local cargo packages offline, so
`myos-bus.mk` runs `cargo vendor` before the build (it needs network access
the first time). protobuf-java is compiled from the protobuf source tree
Buildroot already downloads for host-protobuf, so it always matches protoc.

## Phase 3: core services

Three Rust daemons in `rust/services`, each a service on the bus, replace
BusyBox's `syslogd`, `klogd` and `ifup`/`udhcpc`:

| Service file | Daemon | Bus name | Does |
|---|---|---|---|
| `logd.toml` | `logd` | `log` | collects every log; `/var/log/messages` |
| `netd.toml` | `netd` | `net` | lo, DHCP or static IPv4, `/etc/resolv.conf` |
| `sysinfo.toml` | `sysinfod` | `sysinfo` | CPU, memory, disks, processes |
| `network.toml` | `myos-net wait` | | oneshot: waits (≤ 15 s) until netd is online, so getty starts online |

Their interfaces are in `proto/myos/{log,net,sysinfo}/v1/`.

**Logs.** logd collects from four places:

- **Services:** every service's stdout and stderr. Init now gives each
  service a pipe instead of the console and passes each line on to logd,
  tagged with the service name. A line may start with a syslog priority,
  as with systemd: `<3>failed` is logged at err; the default is info. Until
  logd is listening (early boot, or while it restarts), init holds up to
  1000 lines and also shows them on the console. To keep a service on the
  console, add `output = "console"` to its file. `console = true`
  services, such as getty, are never redirected.
- **`/dev/log`:** BusyBox `logger` and anything else using syslog(3). The
  sender's pid and uid come from the kernel.
- **Kernel messages** from `/dev/kmsg`, from boot onwards. A restarted logd
  doesn't log them twice.
- **The bus:** `log.Write`, for the Phase 6 Java apps.

logd keeps the newest 5000 entries in memory for queries and writes all of
them to `/var/log/messages`, rotating to `messages.1` at 1 MiB. `/var/log`
is now on disk, not a link to `/tmp`, so logs survive a reboot. Times are
UTC.

```sh
myos-log                      # last 50 entries
myos-log -f                   # follow
myos-log -t netd -p warning   # one service, warnings and worse
myos-log -g eth0 -n 200       # containing "eth0"
myos-log write -t me hello    # add an entry over the bus
logger -t me hello            # ... or through /dev/log
myos-log stats
```

Non-root users only see entries written with their own uid.

**Network.** netd brings up lo as 127.0.0.1/8 and configures the interfaces
in `/etc/myos/netd.toml`. The default file runs DHCP on eth0:

```toml
[eth0]
method = "dhcp"

[eth1]                          # an example static interface
method = "static"
address = "192.168.1.10/24"
gateway = "192.168.1.1"         # optional
dns = ["192.168.1.1"]           # optional
```

Without the file, netd runs DHCP on every Ethernet interface. Interfaces that
appear later are picked up within 2 s. The DHCP client renews at T1, rebinds
at T2, and drops the address when the lease runs out. If the link goes down
and comes back, it confirms the lease again. After editing the file, run
`initctl restart netd`.

```sh
myos-net                      # interfaces, addresses, gateway, DNS, leases
myos-net renew eth0           # root only
myos-net wait --timeout 10
```

**System information:**

```sh
myos-sysinfo                  # summary
myos-sysinfo cpu | mem | disks [-a] | ps
```

CPU figures come from samples taken once a second. Phase 4's `ps` and `top`
will call `sysinfo.Processes` directly.

On your machine:

```sh
make test-services            # busd + logd + sysinfod on private sockets, netd in a
                              # network namespace with dnsmasq; ~1 min, needs sudo
cd rust/services && cargo test
make test-init                # now also checks that service output is forwarded
make rebuild-myos-services rebuild-myos-init && make test
```

`make test-services` needs `dnsmasq` and `ip` for the netd part (`apt install
dnsmasq-base iproute2`). Without them, it skips that part.

**Build notes.** The crate depends on `../bus` and reuses
`../init/src/toml.rs`, so `build/package/myos-services` copies all of `rust/`
and builds in `services/`. Like `myos-bus`, it vendors its crates before the
build. The C++, Python and Java bindings still cover only `bus/v1`. Phase 4
adds `sysinfo` for C++, and Phase 5 adds `log` and `net` for Python.
