################################################################################
#
# myos-services
#
################################################################################

MYOS_SERVICES_VERSION = 0.1.0
# The crate is rust/services, but it depends on ../bus (path dependency) and
# netd reuses ../init/src/toml.rs, so Buildroot copies all of rust/ and
# builds in the services subdirectory.
MYOS_SERVICES_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../rust
MYOS_SERVICES_SITE_METHOD = local
MYOS_SERVICES_SUBDIR = services
MYOS_SERVICES_LICENSE = MIT
MYOS_SERVICES_DEPENDENCIES = host-protobuf

# Developers' build output must not be copied into Buildroot's tree.
MYOS_SERVICES_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude target --exclude VENDOR --exclude .cargo

MYOS_SERVICES_CARGO_ENV = \
	PROTOC=$(HOST_DIR)/bin/protoc \
	MYOS_PROTO_DIR=$(BR2_EXTERNAL_MYOS_PATH)/../proto

# As for myos-bus: local cargo packages are built --offline, so vendor the
# crates in Cargo.lock first (network access the first time, or a warm
# $(BR_CARGO_HOME) cache). Path dependencies (../bus) aren't vendored.
define MYOS_SERVICES_VENDOR_CRATES
	cd $(@D)/services && rm -rf VENDOR .cargo && mkdir .cargo && \
	$(HOST_MAKE_ENV) CARGO_HOME=$(BR_CARGO_HOME) \
		cargo vendor --locked --versioned-dirs VENDOR > .cargo/config.toml
endef
MYOS_SERVICES_PRE_BUILD_HOOKS += MYOS_SERVICES_VENDOR_CRATES

# Keep logs on disk across reboots: until Phase 3 /var/log was a link to
# /tmp (tmpfs). A hook rather than the overlay, so an existing output/target
# from an earlier build is fixed too.
define MYOS_SERVICES_PERSISTENT_VAR_LOG
	if [ -L $(TARGET_DIR)/var/log ]; then rm $(TARGET_DIR)/var/log; fi
	mkdir -p $(TARGET_DIR)/var/log
endef
MYOS_SERVICES_TARGET_FINALIZE_HOOKS += MYOS_SERVICES_PERSISTENT_VAR_LOG

# cargo-package installs every [[bin]] to /usr/bin: logd, netd, sysinfod,
# myos-log, myos-net, myos-sysinfo, and jrun (Phase 6).
$(eval $(cargo-package))
