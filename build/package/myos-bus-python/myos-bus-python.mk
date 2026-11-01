################################################################################
#
# myos-bus-python
#
################################################################################

MYOS_BUS_PYTHON_VERSION = 0.1.0
MYOS_BUS_PYTHON_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../python/bus
MYOS_BUS_PYTHON_SITE_METHOD = local
MYOS_BUS_PYTHON_LICENSE = MIT
# python-protobuf is the same release as host-protobuf, so the generated
# code always matches the runtime.
MYOS_BUS_PYTHON_DEPENDENCIES = python3 python-protobuf host-protobuf
MYOS_BUS_PYTHON_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude /myos/bus/v1 --exclude /myos/log --exclude /myos/net --exclude /myos/sysinfo --exclude __pycache__

MYOS_BUS_PYTHON_MAKE_OPTS = \
	PROTOC=$(HOST_DIR)/bin/protoc \
	PROTO_DIR=$(BR2_EXTERNAL_MYOS_PATH)/../proto

define MYOS_BUS_PYTHON_BUILD_CMDS
	$(MAKE) -C $(@D) $(MYOS_BUS_PYTHON_MAKE_OPTS) clean all
endef

define MYOS_BUS_PYTHON_INSTALL_TARGET_CMDS
	$(MAKE) -C $(@D) $(MYOS_BUS_PYTHON_MAKE_OPTS) install \
		DESTDIR=$(TARGET_DIR) \
		SITE=/usr/lib/python$(PYTHON3_VERSION_MAJOR)/site-packages \
		BINDIR=/usr/bin
endef

$(eval $(generic-package))
