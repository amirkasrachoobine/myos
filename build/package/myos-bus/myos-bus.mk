################################################################################
#
# myos-bus
#
################################################################################

MYOS_BUS_VERSION = 0.1.0
MYOS_BUS_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../rust/bus
MYOS_BUS_SITE_METHOD = local
MYOS_BUS_LICENSE = MIT
MYOS_BUS_DEPENDENCIES = host-protobuf

# A developer's own build output must not be copied into Buildroot's tree.
MYOS_BUS_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude /target --exclude /vendor --exclude /.cargo

# build.rs runs protoc on the shared proto/ directory, which isn't part of
# the copy Buildroot builds from.
MYOS_BUS_CARGO_ENV = \
	PROTOC=$(HOST_DIR)/bin/protoc \
	MYOS_PROTO_DIR=$(BR2_EXTERNAL_MYOS_PATH)/../proto

# Buildroot vendors crates only for packages it downloads; local packages
# are built --offline. So vendor the crates in Cargo.lock here, into the
# build directory (needs network access, or a warm $(BR_CARGO_HOME) cache).
define MYOS_BUS_VENDOR_CRATES
	cd $(@D) && rm -rf VENDOR .cargo && mkdir .cargo && \
	$(HOST_MAKE_ENV) CARGO_HOME=$(BR_CARGO_HOME) \
		cargo vendor --locked --versioned-dirs VENDOR > .cargo/config.toml
endef
MYOS_BUS_PRE_BUILD_HOOKS += MYOS_BUS_VENDOR_CRATES

# cargo-package installs every [[bin]]: /usr/bin/busd and /usr/bin/myos-bus.
$(eval $(cargo-package))
