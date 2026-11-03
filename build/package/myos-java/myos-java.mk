################################################################################
#
# myos-java
#
################################################################################

MYOS_JAVA_VERSION = 0.1.0
MYOS_JAVA_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../java
MYOS_JAVA_SITE_METHOD = local
MYOS_JAVA_LICENSE = MIT, GPL-2.0 with Classpath exception (the JDK)
# openjdk: its jmods. host-openjdk-bin: javac and jlink on the build machine.
# myos-bus-java: myos-bus.jar and protobuf-java, from its build directory.
MYOS_JAVA_DEPENDENCIES = openjdk host-openjdk-bin myos-bus-java
MYOS_JAVA_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = --exclude build

MYOS_JAVA_JDK_BIN = $(HOST_OPENJDK_BIN_ROOT_DIR)/bin
MYOS_JAVA_JAR_OPTS = \
	JAVAC=$(MYOS_JAVA_JDK_BIN)/javac \
	JAR=$(MYOS_JAVA_JDK_BIN)/jar \
	JAVA_RELEASE=$(HOST_OPENJDK_BIN_VERSION_MAJOR) \
	BUS_JAR=$(MYOS_BUS_JAVA_DIR)/build/myos-bus.jar \
	PROTOBUF_JAR=$(MYOS_BUS_JAVA_DIR)/build/protobuf-java.jar \
	SDK_JAR=$(@D)/sdk/build/myos-sdk.jar

# jlink from the host JDK, on the jmods of the target JDK Buildroot just
# built: jlink only requires the same feature release (both are 21), and
# the native code it copies comes from the target's jmods.
define MYOS_JAVA_BUILD_CMDS
	$(MAKE) -C $(@D)/sdk $(MYOS_JAVA_JAR_OPTS)
	$(if $(BR2_PACKAGE_MYOS_JAVA_SAMPLE_APP),$(MAKE) -C $(@D)/apps/hello $(MYOS_JAVA_JAR_OPTS) TAR=tar)
	$(MAKE) -C $(@D)/jdk JLINK=$(MYOS_JAVA_JDK_BIN)/jlink \
		JMODS="$$(echo $(OPENJDK_DIR)/build/linux-*-release/images/jdk/jmods)" \
		MODULES=$(call qstrip,$(BR2_PACKAGE_MYOS_JAVA_MODULES)) \
		OUT=$(@D)/jdk/build/jdk
endef

define MYOS_JAVA_INSTALL_TARGET_CMDS
	test ! -e $(TARGET_DIR)/usr/lib/myos/jdk || rm -rf $(TARGET_DIR)/usr/lib/myos/jdk
	mkdir -p $(TARGET_DIR)/usr/lib/myos
	cp -a $(@D)/jdk/build/jdk $(TARGET_DIR)/usr/lib/myos/jdk
	$(MAKE) -C $(@D)/sdk $(MYOS_JAVA_JAR_OPTS) install DESTDIR=$(TARGET_DIR) JAVADIR=/usr/share/java
	$(if $(BR2_PACKAGE_MYOS_JAVA_SAMPLE_APP),$(MAKE) -C $(@D)/apps/hello $(MYOS_JAVA_JAR_OPTS) TAR=tar \
		install DESTDIR=$(TARGET_DIR) APPDIR=/usr/share/myos/apps)
	$(INSTALL) -D -m 644 $(MYOS_JAVA_PKGDIR)/jrun.toml $(TARGET_DIR)/etc/myos/jrun.toml
	mkdir -p $(TARGET_DIR)/var/lib/myos/apps $(TARGET_DIR)/etc/myos/bus/policy.d
endef

# After every package has installed (openjdk puts the full JDK in
# /usr/lib/jvm and links its tools into /usr/bin): drop it, and point
# /usr/bin/java at the trimmed one.
ifeq ($(BR2_PACKAGE_MYOS_JAVA_KEEP_FULL_JDK),)
define MYOS_JAVA_DROP_FULL_JDK
	find $(TARGET_DIR)/usr/bin -maxdepth 1 -type l -lname '*lib/jvm/*' -delete
	test ! -e $(TARGET_DIR)/usr/lib/jvm || rm -rf $(TARGET_DIR)/usr/lib/jvm
	ln -sf ../lib/myos/jdk/bin/java $(TARGET_DIR)/usr/bin/java
endef
MYOS_JAVA_TARGET_FINALIZE_HOOKS += MYOS_JAVA_DROP_FULL_JDK
endif

$(eval $(generic-package))
