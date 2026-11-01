################################################################################
#
# myos-init
#
################################################################################

MYOS_INIT_VERSION = 0.1.0
MYOS_INIT_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../rust/init
MYOS_INIT_SITE_METHOD = local
MYOS_INIT_LICENSE = MIT

# cargo-package installs the binary to /usr/bin/myos-init. The links are
# made at target-finalize so they win over BusyBox's own /sbin/init link,
# even after `make rebuild-busybox`.
define MYOS_INIT_LINKS
	ln -sf ../usr/bin/myos-init $(TARGET_DIR)/sbin/init
	ln -sf myos-init $(TARGET_DIR)/usr/bin/initctl
	mkdir -p $(TARGET_DIR)/etc/myos/services
endef
MYOS_INIT_TARGET_FINALIZE_HOOKS += MYOS_INIT_LINKS

$(eval $(cargo-package))
