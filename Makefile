# myos — top-level build driver (Phase 0)
#
#   make            fetch Buildroot, configure, build everything
#   make run        boot the image in QEMU (serial console in this terminal)
#   make test       automated boot test: waits for login, logs in, runs checks
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
