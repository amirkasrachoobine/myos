################################################################################
#
# myos-hello-cpp
#
################################################################################

MYOS_HELLO_CPP_VERSION = 0.1.0
MYOS_HELLO_CPP_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../cpp/hello
MYOS_HELLO_CPP_SITE_METHOD = local
MYOS_HELLO_CPP_LICENSE = MIT

define MYOS_HELLO_CPP_BUILD_CMDS
	$(TARGET_MAKE_ENV) $(MAKE) -C $(@D) \
		CXX="$(TARGET_CXX)" CXXFLAGS="$(TARGET_CXXFLAGS)" LDFLAGS="$(TARGET_LDFLAGS)"
endef

define MYOS_HELLO_CPP_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/myos-hello-cpp $(TARGET_DIR)/usr/bin/myos-hello-cpp
endef

$(eval $(generic-package))
