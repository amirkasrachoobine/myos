# myos

A headless Linux-based OS with a userland written in Rust, C++, Python and Java.
Phases 0–6 are done: boot, the Rust init, the IPC bus, the core services,
the C++ shell and utilities, the Python admin tools, and the Java app runtime.

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

The image includes OpenJDK 21 for the Phase 6 app runtime, which adds about an
hour to a clean build. `make WITH_JAVA=0` leaves Java out.

## What's in the image

| Piece | Source | Notes |
|---|---|---|
| Kernel | Linux 6.18.7 | Buildroot's QEMU x86_64 config + `build/board/qemu-x86_64/linux.fragment` |
| libc / toolchain | glibc, GCC with C++ | built by Buildroot (Rust and OpenJDK both need glibc) |
| Init (temporary) | BusyBox init + getty on `ttyS0` | replaced by our Rust PID 1 in Phase 1 |
| Rust | `rust/hello` → `/usr/bin/myos-hello-rust` | built with Buildroot's `cargo-package` |
| C++ | `cpp/hello` → `/usr/bin/myos-hello-cpp` | built with the target `g++` |
| Python | Python 3 + `/usr/bin/myos-hello-py` | |
| Java | OpenJDK 21, trimmed with jlink | Phase 6; `WITH_JAVA=0` leaves it out |
| QEMU | built on the host by Buildroot | so you don't depend on the distro's version |

## Layout

```
myos/
├── Makefile                 # driver: fetches Buildroot 2026.02.x (LTS) and builds
├── build/                   # Buildroot external tree (BR2_EXTERNAL=build)
│   ├── configs/             # myos_qemu_x86_64_defconfig, no-java.fragment
│   ├── board/qemu-x86_64/   # kernel fragment, rootfs overlay
│   └── package/             # our Buildroot packages, one per component
├── rust/                    # Rust crates (init, daemons from Phase 1)
├── cpp/                     # C++: bus client (Phase 2), msh + ls/cat/ps/top (Phase 4)
├── python/                  # bus client (Phase 2), myctl + myos-install (Phase 5)
├── java/                    # bus client (Phase 2); SDK, jlink'd JDK, sample app (Phase 6)
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
adds `sysinfo` for C++, and Phase 5 adds `log`, `net` and `sysinfo` for Python.

## Phase 4: the shell and utilities

Root now logs in to **msh**, the myos shell (`cpp/msh`, `/usr/bin/msh`).
`/bin/sh` is still BusyBox ash, so scripts are unaffected. The C++ **ls**,
**cat**, **ps** and **top** (`cpp/utils`) replace BusyBox's applets of the
same names; `busybox ps` and so on still work.

**msh, the language.** Pipes; `;` `&&` `||` `&` `!`; redirections `<` `>`
`>>` `2>` `2>&1` `&>` `<>` `3>&-`; `'single'`, `"double"` and `\`
quoting; `$var` `${var:-default}` `${var:=x}` `${var:+x}` `${var?msg}`
`${#var}` `${var%suffix}` `${var##prefix}`; `$(cmd)` and backticks;
`$((arithmetic))`; `~`; globs `*` `?` `[...]`; `if`/`elif`/`else`,
`while`, `until`, `for x in ...`, `break n`, `continue n`; `{ ...; }` and
`( ... )`; functions with `local` and `return`; `set -e`, `set -x`;
aliases; comments. Every boot-test command from Phases 0–3 now runs through
it. Not supported: here-documents (`<<`), `case`, and bash-only syntax such
as `[[ ]]` and arrays; use `sh` for those.

**msh, interactively:**

| Key | Does |
|---|---|
| ← → Home End, Ctrl-A/E/B/F, Alt-B/F | move |
| Backspace, Del, Ctrl-K/U/W, Alt-D, Ctrl-Y | delete, kill, yank |
| ↑ ↓, Ctrl-P/N | history |
| Ctrl-R | search history as you type; Enter runs it, → edits it |
| Tab | complete a command, file, directory (after `cd`) or `$VAR`; twice lists the choices |
| Ctrl-C | cancel the line, or interrupt the running command (also a shell loop) |
| Ctrl-Z | stop the running job; `fg`, `bg`, `jobs`, `kill %1`, `wait` |
| Ctrl-L | clear the screen |
| Ctrl-D | exit (on an empty line) |

History is saved to `~/.msh_history` as you go (`HISTSIZE`, default 1000),
so it survives reboots; `history`, `!!`, `!n`, `!-n`, `!prefix` and `!$`
work. Commands typed over several lines (`for ...; do` then Enter) are
saved as one line. Background jobs that finish are reported at the next
prompt, and `exit` warns once if jobs are stopped.

On the serial console the kernel doesn't know the terminal's size, so msh
asks the terminal once at startup (a cursor position report) and passes the
answer to the kernel, so `top` and `ls` use the full width too. After
resizing your terminal window, run `resize`.

Startup files: `/etc/mshrc` (prompt and the `ll`/`la`/`l` aliases), then
`~/.mshrc`, for every interactive shell. msh doesn't read `/etc/profile`,
which is sh syntax. Also: `msh -c 'cmds'`, `msh script args`, `msh -n`
(syntax check only), and `help` for the builtins.

**The utilities:**

```sh
ls -la          # also -A -h -n -i -d -R -t -S -r -U -F -p -1 -C --color=auto|always|never
cat -n file     # also -b -s -E -T -v -A -e -t, and - for stdin
ps              # PID USER TIME COMMAND for every process
ps aux          # or -ef, -l, -o pid,user,%cpu,rss,args, -p 1,2 -u root -C logd --sort=-%cpu
top             # P M N T sort, R reverse, c full commands, 1 per CPU, k kill, d delay, q quit
top -b -n 1     # one plain frame, for scripts
```

ls and cat match GNU coreutils' output for these options, colours aside (the
host test compares them byte for byte). `ps` and `top` call `sysinfo.Processes`,
`System`, `Cpu` and `Memory` on the bus, so they show what `myos-sysinfo`
shows, and CPU figures are sysinfod's one-second samples. If sysinfod or
busd is down, they say which (`initctl status sysinfo`); top keeps running
and recovers when sysinfod is back. `cpp/bus` now also generates the C++
types for `sysinfo`, `log` and `net`.

On your machine:

```sh
make test-shell               # msh vs bash, msh on a pseudo-terminal, ls/cat vs GNU,
                              # ps/top vs /proc with busd + sysinfod; under a minute, no root
cmake -S cpp/msh -B /tmp/msh && cmake --build /tmp/msh && /tmp/msh/msh
make rebuild-myos-msh rebuild-myos-utils && make test
```

`make test-shell` needs `cmake`, `bash`, GNU coreutils, the protobuf tools
from Phase 2 and cargo; `pip install pyte` adds the checks of what's drawn
on screen (line editing, top).

**Build notes.** `myos-msh` has no dependencies. Its option
`BR2_PACKAGE_MYOS_MSH_LOGIN_SHELL` (on by default) sets root's shell in
`/etc/passwd` and adds msh to `/etc/shells`; turning it off puts `/bin/sh`
back. `myos-utils` builds against libmyosbus from staging and installs over
BusyBox's links in `/bin` and `/usr/bin`. BusyBox installs with noclobber,
so rebuilding BusyBox later leaves them in place. If msh ever fails to
start, `myos.emergency` on the kernel command line still gives you a
BusyBox shell (see Phase 1).

## Phase 5: the admin tools

Two Python tools in `python/admin` (package `myos.admin`, Buildroot package
`myos-admin`): **myctl** runs the system, **myos-install** puts it on a disk.

**myctl: services.** It talks to init over its control socket, as
`initctl` does, and waits for the result:

```sh
myctl status                  # every service: state, pid, uptime, description
myctl status netd             # exit status 3 if it isn't running (or done)
myctl start|stop|restart netd # prints the state it settled in
myctl disable hello           # sets enabled = false in hello.toml, stops it
myctl enable hello            # enabled = true, starts it (a oneshot runs again)
myctl cat netd                # the service file
myctl reload                  # re-read /etc/myos/services
myctl poweroff | reboot
```

**myctl: logs**, from logd over the bus (`log.Query`), in `myos-log`'s format:

```sh
myctl logs                    # the last 50 entries
myctl logs netd -n 200        # one service (tag)
myctl logs -p warning -g eth0 # warnings and worse, containing "eth0"
myctl logs -f                 # follow; survives logd and busd restarts
```

**myctl: settings.** Each setting lives in an ordinary file; myctl edits
that file in place, keeping its comments, and applies the change:

| Key | File | Applied by |
|---|---|---|
| `hostname` | `/etc/hostname`, and the `127.0.1.1` line of `/etc/hosts` | setting it now |
| `net.IFACE.method` | `/etc/myos/netd.toml` | restarting netd |
| `net.IFACE.address`, `.gateway`, `.dns` | same (static only) | restarting netd |
| `service.NAME.enabled` | `/etc/myos/services/NAME.toml` | `myctl reload` |

```sh
myctl config                  # every setting and its value
myctl get net.eth0.method
myctl set hostname box
myctl set net.eth1.method static net.eth1.address 192.168.1.10/24 \
          net.eth1.gateway 192.168.1.1 net.eth1.dns 1.1.1.1,9.9.9.9
myctl set net.eth1.method dhcp    # also removes eth1's address, gateway, dns
myctl unset net.eth1              # netd stops managing eth1
myctl set --no-apply ...          # write the files only; applied at the next boot
```

Several `KEY VALUE` pairs are one change: they are checked together, with
the same rules netd and init apply, and nothing is written if any is wrong
(so a static interface needs its address in the same command). Services
and settings need root; anyone can read their own log entries.

**myos-install** copies the running system onto another disk and makes it
bootable on a BIOS PC (or QEMU's default machine):

```sh
myos-install --list                     # disks, sizes, what's mounted
myos-install /dev/vdb                   # asks you to type "yes" first
myos-install --yes --hostname box /dev/vdb
myos-install --image-size 2G disk.img   # into an image file (loop device)
```

1. Writes an MBR: one bootable Linux partition from 1 MiB to the end
   (`--root-size 2G` for less), GRUB's boot code in the MBR, and GRUB's core
   image in the gap before the partition. The start and end of the disk are
   zeroed first, so no old partition table survives.
2. `mkfs.ext4 -L myos-root` on the partition.
3. Copies `/` like `cp -ax`: owners, modes, times, links, device nodes and
   extended attributes, one filesystem only. `/proc`, `/sys`, `/dev`,
   `/run`, `/tmp` and `/var/log` come over empty; shell history is left out.
4. Writes `/boot/grub/grub.cfg` with `root=PARTUUID=<disk id>-01`, so the
   kernel finds its root whatever the disk is called, sets the hostname and
   records the install in `/etc/myos/installed`. The menu has a second entry
   that boots to the emergency shell.

It refuses a disk that is mounted or in use as swap (so never the one
you're running from) and a disk too small for the system, and it won't
erase anything without `--yes` or a typed "yes" on a terminal. It needs no
fdisk, losetup or grub-install: the partition table, the loop device and
GRUB's sectors are written directly. The kernel it installs is
`/boot/bzImage`, now part of the image (`BR2_LINUX_KERNEL_INSTALL_TARGET`).

**Try it in QEMU:**

```sh
truncate -s 2G output/disk.img
make run EXTRA_DISK=$PWD/output/disk.img    # the disk is /dev/vdb inside
#   myos-install --yes /dev/vdb && poweroff
make run DISK=$PWD/output/disk.img          # boots it through GRUB, like a PC
```

`make test` now does the same: after the earlier checks and the myctl ones,
it installs onto a fresh second disk (`output/images/install-test.img`),
powers off, boots that disk on its own and checks it came up with its own
hostname, root by PARTUUID, every service running and fresh logs. That is
the Phase 5 done-check. The two console logs are `output/boot_test.log` and
`output/boot_test_installed.log`.

The same disk works in virt-manager: import `disk.img` as an existing disk,
with no direct kernel boot.

On your machine:

```sh
make test-admin               # ~1 min, uses sudo; see below
make rebuild-myos-admin && make test
```

`make test-admin` checks the settings editor against init's own parser,
`myctl logs` against busd and logd, `myctl`'s service commands against
myos-init as PID 1 in namespaces, and the MBR and copy code. Then it runs
`myos-install` onto a loop-mounted image file and boots that in QEMU through
GRUB. That last part needs `grub-pc-bin`, `busybox-static`,
`qemu-system-x86` and a readable kernel (`MYOS_TEST_KERNEL=/path/to/bzImage`,
default: the newest readable `/boot/vmlinuz-*`), and skips itself without
them.

**Build notes.** The defconfig turns on GRUB for BIOS
(`BR2_TARGET_GRUB2_I386_PC`) with the serial console built into the core
image, so the menu shows on `ttyS0`. GRUB doesn't boot the development image,
since `make run` still loads the kernel directly. `myos-admin` copies GRUB's
`boot.img` and the core image to `/usr/lib/myos/boot/` for myos-install to
write. `myos-admin` also selects e2fsprogs for `mkfs.ext4`. The kernel
fragment adds MBR and GPT partition support and loop devices.
`myos-bus-python` now also generates the `log`, `net` and `sysinfo` modules
(`myos.log.v1.log_pb2` and so on). UEFI isn't supported yet: installed disks
boot through the BIOS (SeaBIOS, as QEMU and virt-manager use by default).

## Phase 6: the Java app runtime

Java apps are packages you install with **jrun**. Each app runs as a service
under its own user, in a sandbox that gives it only what its manifest asks
for. A small SDK gives it the bus: logs, system information, the network
and its own files.

```
java/jdk/          the runtime: OpenJDK 21 trimmed with jlink, /usr/lib/myos/jdk (~40 MB)
java/sdk/          myos-sdk.jar: the SDK (org.myos.sdk) and the launcher (org.myos.runtime.Main)
java/bus/          myos-bus.jar, now with the log, net and sysinfo messages too
java/apps/hello/   the sample app, /usr/share/myos/apps/org.myos.hello.mapp
rust/services/src/bin/jrun/   jrun: install, run, remove (Rust; it needs root)
```

**Try it:**

```sh
jrun install /usr/share/myos/apps/org.myos.hello.mapp
myctl status app.org.myos.hello     # a service like any other
myctl logs app.org.myos.hello       # its log entries and its stdout
jrun list
jrun info org.myos.hello            # manifest, user, permissions, state
jrun remove org.myos.hello          # also --keep-data
```

**A package** (`.mapp`) is an uncompressed tar archive with `app.toml` at the
top and the app's jars:

```toml
id = "org.myos.hello"            # reverse-DNS; the service is app.<id>
name = "Hello"
version = "1.0.0"
main = "org.myos.hello.Hello"    # implements org.myos.sdk.App, or has main(String[])
classpath = ["hello.jar"]        # default ["app.jar"]
args = ["--interval", "30"]
permissions = ["log", "sysinfo"]
memory = 32                      # MiB of heap (-Xmx), 16–2048, default 64
service = true                   # false: run it by hand with `jrun run ID`
restart = "on-failure"
```

`jrun pack DIR -o app.mapp` makes one from a directory, and `jrun check`
validates a package. `jrun install` refuses anything it can't extract
safely: links, absolute paths, `..`, duplicates, and compressed archives.

**Permissions.** The system enforces them, not just the SDK:

| Permission | Allows | Enforced by |
|---|---|---|
| `log` | `log.Write` and reading its own entries | busd |
| `sysinfo` | the `sysinfo` service | busd |
| `netinfo` | the `net` service (interface status) | busd |
| `network` | connections; without it the app has a network namespace with only `lo` | the kernel |
| `call:SERVICE` | another bus service, e.g. `call:app.org.example.notes` or `call:app.org.example.*` | busd |

Every app may also call `bus` and serve its own name, `app.<id>` (and names
under it). Nothing else.

**What install does:**

- **A user:** `app_hello`, with a uid from 20000–29999.
- **Data:** `/var/lib/myos/apps/<id>/data`, owned by the app's user, mode 0700. The package goes next to it in `pkg/`, owned by root.
- **Bus policy:** `/etc/myos/bus/policy.d/<uid>`, listing the services the app may call.
- **Service file:** `/etc/myos/services/app.<id>.toml`. Then init reloads and starts the app.

An upgrade keeps the app's uid and data, keeps `enabled` as it was, and
restarts the app if it was running. `--no-start` installs the app disabled
(`myctl enable app.<id>` to start it).

**The sandbox.** `jrun run ID` is what the service runs. As root, it sets up
the following, then starts the JVM:

- **Namespaces:** a private mount namespace with fresh tmpfs on `/tmp`,
  `/var/tmp` and `/dev/shm`. Without `network`, also a network namespace
  with only `lo`, brought up.
- **Limits:** 4096 open files, 1024 processes, no core dumps.
- **Privileges:** it switches to the app's uid and gid with no supplementary
  groups, checks that root can't be regained, and sets `no_new_privs`.
- **Environment:** the app starts in its data directory with a clean
  environment.

busd enforces the bus side: a connection whose uid is in the app range may
call only what its policy file lists. If the file is missing, or not owned by
root, the app gets nothing but `bus.Ping`. `jrun remove` stops the app, kills
anything left running as its uid, and deletes its files, policy and user.

**Writing an app**, against the SDK:

```java
public final class Notes implements org.myos.sdk.App {
    public void run(AppContext app) throws Exception {
        app.log().info("started, run %s", app.files().read("runs").orElse("0"));
        app.serve(req -> ...);                          // answer calls to app.<id> on the bus
        while (app.sleep(Duration.ofSeconds(10))) {     // false once the service is stopped
            MemoryReply m = app.sysinfo().memory();     // needs "sysinfo"
            app.files().write("last", m.getUsedKb() + "\n");
        }
    }
}
```

| SDK | Gives |
|---|---|
| `app.log()` | `info`/`notice`/`warning`/`error`/`debug`, tagged `app.<id>`; falls back to stderr if logd is down |
| `app.files()` | its data directory: `read`, `write` (atomic), `append`, `list`, `delete`; paths can't leave it |
| `app.sysinfo()` | `system()`, `cpu()`, `memory()`, `disks()`, `processes()` |
| `app.net()` | `connect()`, `http()` (java.net.http), `interfaces()` (needs `netinfo`) |
| `app.serve(handler)` | serves `app.<id>`; registers again if busd restarts |
| `app.connect()` | a raw `BusClient`, for other apps' services |
| `app.args()`, `id()`, `version()`, `permissions()`, `stopping()`, `onStop()` | |

The SDK throws `PermissionException` before trying something the manifest
doesn't allow, so the error says which permission is missing. Stdout and
stderr go to logd through init, like any service's output. On
`myctl stop`, the JVM gets SIGTERM: `stopping()` turns true, `sleep()`
returns false, and the app has 5 seconds to return from `run()`. The sample
app in `java/apps/hello` uses all of this. Build an app on your machine
like the sample's Makefile does: `javac` against `myos-sdk.jar`,
`myos-bus.jar` and protobuf-java.

On your machine:

```sh
make test-java                # ~1 min, uses sudo; see below
make rebuild-myos-java rebuild-myos-services && make test
```

`make test-java` builds the jars, the sample app and a jlink'd JDK, and
checks jrun's package handling. Then it runs myos-init as PID 1 in
namespaces with busd, logd and sysinfod, and installs the sample app with
jrun. That covers the Phase 6 done-check: the app runs as a service, under
its own uid, and its entries reach logd. A probe app (`tests/java/probe`)
then tries what it may not do: other services, other apps' names and files,
the network, `/etc`, its own package. The check passes only if the system
refuses all of it. Then the test upgrades, stops, disables and removes the
apps. It needs a JDK 21 with jmods, `protoc`, cargo, GNU tar and
protobuf-java (`PROTOBUF_JAR=...`, `make -C java/bus fetch-protobuf`, or
Debian's `libprotobuf-java`). Your `/etc/passwd` and `/etc/myos` are not
touched: the test binds copies over them inside the namespace.

`make test` now also installs the sample app in the VM, checks its user,
uid, namespace and log entries, and checks that it still runs on the
installed disk.

**Build notes.** The defconfig now builds OpenJDK 21 as a full JDK (for its
jmods). `myos-java` runs the host JDK's jlink on those jmods, which needs the
same feature release (both are 21.0.4). It then removes the full JDK from the
target in a finalize hook and points `/usr/bin/java` at the trimmed one.
`BR2_PACKAGE_MYOS_JAVA_KEEP_FULL_JDK` keeps the full JDK, and
`BR2_PACKAGE_MYOS_JAVA_MODULES` picks the modules. The libraries OpenJDK
pulls in (alsa, cups, fontconfig) stay in the image. The kernel fragment adds
`CONFIG_NAMESPACES` and `CONFIG_NET_NS`: without network namespaces, jrun
refuses to start apps that lack `network`. log.proto's `StatsReply.file_bytes`
is now `file_size`: protobuf-java generates `getFileBytes()` for the `file`
field too. The field number is the same, so the wire format doesn't change.
myctl's `service.NAME.enabled` now accepts names with dots.
