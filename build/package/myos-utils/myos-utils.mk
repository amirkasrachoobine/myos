################################################################################
#
# myos-utils
#
################################################################################

MYOS_UTILS_VERSION = 0.1.0
MYOS_UTILS_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../cpp/utils
MYOS_UTILS_SITE_METHOD = local
MYOS_UTILS_LICENSE = MIT
# A developer's own build directory must not be copied into Buildroot's.
MYOS_UTILS_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude /build
# libmyosbus, its headers and myos-bus.pc come from staging.
MYOS_UTILS_DEPENDENCIES = myos-bus-cpp host-pkgconf

# Put ours where BusyBox puts its links, replacing them rather than
# sitting next to them on PATH. BusyBox installs with noclobber, so a
# later BusyBox rebuild leaves these alone.
define MYOS_UTILS_INSTALL_TARGET_CMDS
	for t in ls cat ps; do \
		rm -f $(TARGET_DIR)/bin/$$t; \
		$(INSTALL) -D -m 0755 $(MYOS_UTILS_BUILDDIR)/$$t $(TARGET_DIR)/bin/$$t || exit 1; \
	done
	rm -f $(TARGET_DIR)/usr/bin/top
	$(INSTALL) -D -m 0755 $(MYOS_UTILS_BUILDDIR)/top $(TARGET_DIR)/usr/bin/top
endef

$(eval $(cmake-package))
