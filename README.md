# myos

A headless Linux-based OS with a userland written in Rust, C++, Python and Java.
This is **Phase 0: toolchain and boot**.

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
