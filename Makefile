# myos — top-level build driver (Phase 0)
#
#   make            fetch Buildroot, configure, build everything
#   make run        boot the image in QEMU (serial console in this terminal)
#   make test       automated boot test: waits for login, logs in, runs checks,
#                   installs to a second disk and boots that (Phase 5)
#   make menuconfig / linux-menuconfig / savedefconfig
#
# Options:
#   WITH_JAVA=0     leave out OpenJDK and the Phase 6 Java app runtime (saves
#                   about an hour on a clean build)
#   O=<dir>         build output directory (default: ./output)

BR_VERSION   ?= 2026.02.x
BR_URL       ?= https://github.com/buildroot/buildroot.git
BR_DIR       ?= $(CURDIR)/buildroot
O            ?= $(CURDIR)/output
EXTERNAL     := $(CURDIR)/build
DEFCONFIG    := myos_qemu_x86_64_defconfig
NO_JAVA_FRAG := $(EXTERNAL)/configs/no-java.fragment

BR_MAKE = $(MAKE) -C $(BR_DIR) O=$(O) BR2_EXTERNAL=$(EXTERNAL)

.PHONY: all buildroot config build menuconfig linux-menuconfig savedefconfig \
        run test rebuild-% clean distclean

all: build

$(BR_DIR)/Makefile:
	git clone --depth 1 -b $(BR_VERSION) $(BR_URL) $(BR_DIR)

buildroot: $(BR_DIR)/Makefile

$(O)/.config: $(EXTERNAL)/configs/$(DEFCONFIG) | buildroot
	$(BR_MAKE) $(DEFCONFIG)
ifeq ($(WITH_JAVA),0)
	cat $(NO_JAVA_FRAG) >> $(O)/.config
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

# Phase 6: the Java app runtime on this machine. Builds the bus jar, the SDK,
# the sample app and a jlink'd JDK, checks jrun's package handling, then runs
# myos-init as PID 1 in namespaces with busd, logd and sysinfod, installs the
# sample app and a probe app with jrun, and checks that they run as services
# under their own uids, log through logd and get only what their manifests
# ask for. Needs a JDK 21 with jmods, protoc, cargo, GNU tar and protobuf-java
# (PROTOBUF_JAR=..., `make -C java/bus fetch-protobuf`, or Debian's
# libprotobuf-java). Uses sudo. About a minute.
.PHONY: test-java
test-java:
	sudo env PROTOBUF_JAR="$(PROTOBUF_JAR)" python3 tests/java_test.py
