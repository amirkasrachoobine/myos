################################################################################
#
# myos-bus-java
#
################################################################################

MYOS_BUS_JAVA_VERSION = 0.1.0
MYOS_BUS_JAVA_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../java/bus
MYOS_BUS_JAVA_SITE_METHOD = local
MYOS_BUS_JAVA_LICENSE = MIT, BSD-3-Clause (protobuf-java)
MYOS_BUS_JAVA_DEPENDENCIES = openjdk host-openjdk-bin host-protobuf
MYOS_BUS_JAVA_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude /build

MYOS_BUS_JAVA_MAKE_OPTS = \
	PROTOC=$(HOST_DIR)/bin/protoc \
	PROTO_DIR=$(BR2_EXTERNAL_MYOS_PATH)/../proto \
	JAVAC=$(HOST_OPENJDK_BIN_ROOT_DIR)/bin/javac \
	JAR=$(HOST_OPENJDK_BIN_ROOT_DIR)/bin/jar \
	JAVA_RELEASE=$(HOST_OPENJDK_BIN_VERSION_MAJOR)

# protobuf-java comes from host-protobuf's own (hash-checked) source tree,
# compiled with the host protoc of the same release.
define MYOS_BUS_JAVA_BUILD_CMDS
	$(MAKE) -C $(@D) $(MYOS_BUS_JAVA_MAKE_OPTS) clean
	$(MAKE) -C $(@D) $(MYOS_BUS_JAVA_MAKE_OPTS) protobuf-from-source PROTOBUF_SRC=$(HOST_PROTOBUF_SRCDIR)
	$(MAKE) -C $(@D) $(MYOS_BUS_JAVA_MAKE_OPTS) all
endef

define MYOS_BUS_JAVA_INSTALL_TARGET_CMDS
	$(MAKE) -C $(@D) $(MYOS_BUS_JAVA_MAKE_OPTS) install \
		DESTDIR=$(TARGET_DIR) JAVADIR=/usr/share/java BINDIR=/usr/bin
endef

$(eval $(generic-package))
