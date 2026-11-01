################################################################################
#
# myos-msh
#
################################################################################

MYOS_MSH_VERSION = 0.1.0
MYOS_MSH_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../cpp/msh
MYOS_MSH_SITE_METHOD = local
MYOS_MSH_LICENSE = MIT
# A developer's own build directory must not be copied into Buildroot's.
MYOS_MSH_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude /build

# A hook rather than the rootfs overlay, so that turning the option off
# puts /bin/sh back on an existing output/target too.
ifeq ($(BR2_PACKAGE_MYOS_MSH_LOGIN_SHELL),y)
define MYOS_MSH_LOGIN_SHELL
	grep -qsE '^/usr/bin/msh$$' $(TARGET_DIR)/etc/shells \
		|| echo "/usr/bin/msh" >> $(TARGET_DIR)/etc/shells
	$(SED) 's#^\(root:[^:]*:0:0:[^:]*:[^:]*:\).*#\1/usr/bin/msh#' $(TARGET_DIR)/etc/passwd
endef
else
define MYOS_MSH_LOGIN_SHELL
	$(SED) 's#^\(root:[^:]*:0:0:[^:]*:[^:]*:\)/usr/bin/msh$$#\1/bin/sh#' $(TARGET_DIR)/etc/passwd
endef
endif
MYOS_MSH_TARGET_FINALIZE_HOOKS += MYOS_MSH_LOGIN_SHELL

$(eval $(cmake-package))
