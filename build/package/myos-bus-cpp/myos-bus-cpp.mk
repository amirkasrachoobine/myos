################################################################################
#
# myos-bus-cpp
#
################################################################################

MYOS_BUS_CPP_VERSION = 0.1.0
MYOS_BUS_CPP_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../cpp/bus
MYOS_BUS_CPP_SITE_METHOD = local
MYOS_BUS_CPP_LICENSE = MIT
MYOS_BUS_CPP_DEPENDENCIES = protobuf host-protobuf
MYOS_BUS_CPP_INSTALL_STAGING = YES
MYOS_BUS_CPP_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude /build

MYOS_BUS_CPP_CONF_OPTS = \
	-DPROTOC=$(HOST_DIR)/bin/protoc \
	-DMYOS_PROTO_DIR=$(BR2_EXTERNAL_MYOS_PATH)/../proto

$(eval $(cmake-package))
