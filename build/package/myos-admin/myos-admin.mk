################################################################################
#
# myos-admin
#
################################################################################

MYOS_ADMIN_VERSION = 0.5.0
MYOS_ADMIN_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../python/admin
MYOS_ADMIN_SITE_METHOD = local
MYOS_ADMIN_LICENSE = MIT
MYOS_ADMIN_DEPENDENCIES = python3 myos-bus-python e2fsprogs
MYOS_ADMIN_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude __pycache__

define MYOS_ADMIN_INSTALL_TARGET_CMDS
	$(MAKE) -C $(@D) install \
		DESTDIR=$(TARGET_DIR) \
		SITE=/usr/lib/python$(PYTHON3_VERSION_MAJOR)/site-packages \
		BINDIR=/usr/bin
endef

# myos-install writes GRUB's first sector (boot.img) into the MBR and its
# core image (grub.img, built by the grub2 package with the modules and
# prefix in the defconfig) into the gap before the first partition.
ifeq ($(BR2_TARGET_GRUB2_I386_PC),y)
MYOS_ADMIN_DEPENDENCIES += grub2
define MYOS_ADMIN_INSTALL_GRUB_IMAGES
	$(INSTALL) -D -m 0644 $(GRUB2_DIR)/build-i386-pc/grub-core/boot.img \
		$(TARGET_DIR)/usr/lib/myos/boot/boot.img
	$(INSTALL) -D -m 0644 $(BINARIES_DIR)/grub.img \
		$(TARGET_DIR)/usr/lib/myos/boot/core.img
endef
MYOS_ADMIN_POST_INSTALL_TARGET_HOOKS += MYOS_ADMIN_INSTALL_GRUB_IMAGES
endif

$(eval $(generic-package))
