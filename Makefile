# myos — top-level build driver (Phase 0)
#
#   make            fetch Buildroot, configure, build everything
#   make run        boot the image in QEMU (serial console in this terminal)
#   make test       automated boot test: waits for login, logs in, runs checks,
#                   installs to a second disk and boots that (Phase 5)
#   make menuconfig / linux-menuconfig / savedefconfig
#
# Options:
#   WITH_JAVA=1     also build OpenJDK into the image (slow; needed from Phase 6)
#   O=<dir>         build output directory (default: ./output)

BR_VERSION   ?= 2026.02.x
BR_URL       ?= https://github.com/buildroot/buildroot.git
BR_DIR       ?= $(CURDIR)/buildroot
O            ?= $(CURDIR)/output
EXTERNAL     := $(CURDIR)/build
DEFCONFIG    := myos_qemu_x86_64_defconfig
JAVA_FRAG    := $(EXTERNAL)/configs/java.fragment

BR_MAKE = $(MAKE) -C $(BR_DIR) O=$(O) BR2_EXTERNAL=$(EXTERNAL)

.PHONY: all buildroot config build menuconfig linux-menuconfig savedefconfig \
        run test rebuild-% clean distclean

all: build

$(BR_DIR)/Makefile:
	git clone --depth 1 -b $(BR_VERSION) $(BR_URL) $(BR_DIR)

buildroot: $(BR_DIR)/Makefile

$(O)/.config: $(EXTERNAL)/configs/$(DEFCONFIG) | buildroot
	$(BR_MAKE) $(DEFCONFIG)
ifeq ($(WITH_JAVA),1)
	cat $(JAVA_FRAG) >> $(O)/.config
	$(BR_MAKE) olddefconfig
endif

config: $(O)/.config

build: config
	$(BR_MAKE)

menuconfig linux-menuconfig: config
	$(BR_MAKE) $@

savedefconfig: config
	$(BR_MAKE) savedefconfig BR2_DEFCONFIG=$(EXTERNAL)/configs/$(DEFCONFIG)

# Rebuild one package after editing its source, e.g. `make rebuild-myos-hello-rust`
rebuild-%: config
	$(BR_MAKE) $*-dirclean $*

run:
	O=$(O) ./scripts/run-qemu.sh

test:
	O=$(O) python3 tests/boot_test.py

clean:
	rm -rf $(O)

distclean: clean
	rm -rf $(BR_DIR)

# Phase 1: run myos-init as PID 1 in namespaces on this machine (no QEMU,
# no Buildroot build). Needs sudo for the namespaces and mounts.
.PHONY: test-init
test-init:
	cd rust/init && cargo test --release && cargo build --release
	sudo python3 tests/init_ns_test.py

# Phase 2: build busd and the Rust, C++, Python and Java bus clients on this
# machine and check each one can ping the broker. No QEMU, about a minute.
.PHONY: test-bus
test-bus:
	python3 tests/bus_test.py

# Phase 3: run busd, logd and sysinfod on this machine against private
# sockets, and netd in a network namespace against dnsmasq. No QEMU, ~1 min.
# The netd part needs root, iproute2 and dnsmasq (apt install dnsmasq-base).
.PHONY: test-services
test-services:
	cd rust/bus && cargo build --release
	cd rust/services && cargo test --release && cargo build --release
	sudo python3 tests/services_test.py --no-build

# Phase 4: build msh and the utilities on this machine and test them: msh
# against bash and on a pseudo-terminal (editing, history, completion, job
# control); ls and cat against GNU coreutils; ps and top against busd and
# sysinfod on a private socket. No QEMU, no root, under a minute.
.PHONY: test-shell
test-shell:
	python3 tests/msh_test.py
	python3 tests/utils_test.py

# Phase 5: test myctl and myos-install on this machine. Settings, logs
# (busd + logd on private sockets), MBR and copy checks run as you; with
# sudo, also myctl against myos-init as PID 1 in namespaces, and
# myos-install onto a loop-mounted image file that is then booted in QEMU
# through GRUB (needs grub-pc-bin, busybox-static, qemu-system-x86 and a
# readable kernel: MYOS_TEST_KERNEL=/path/to/bzImage). No Buildroot, ~1 min.
.PHONY: test-admin
test-admin:
	$(MAKE) -s -C python/bus
	cd rust/init && cargo build --release
	cd rust/bus && cargo build --release
	cd rust/services && cargo build --release
	sudo env MYOS_TEST_KERNEL="$(MYOS_TEST_KERNEL)" python3 tests/admin_test.py --no-build
