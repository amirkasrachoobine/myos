# The Java runtime for apps: a JDK trimmed with jlink to the modules the SDK
# and apps need, about 50 MB instead of the full JDK's ~300 MB.
#
#   make -C java/jdk                     # from the JDK on PATH, into build/jdk
#   make -C java/jdk JLINK=/path/bin/jlink JMODS=/path/jmods OUT=/somewhere
#
# Buildroot runs the host JDK's jlink (host-openjdk-bin) on the jmods of the
# target OpenJDK it built; jlink only needs the same feature release (21).
# To give apps more of the JDK, add modules to MODULES (`java --list-modules`
# on a full JDK lists them).

JLINK ?= jlink
JMODS ?= $(dir $(shell readlink -f $$(command -v $(JLINK))))../jmods
OUT ?= build/jdk

# java.base, java.logging, jdk.unsupported: protobuf-java and the SDK.
# java.net.http: Net.http() in the SDK. jdk.crypto.ec: TLS with EC keys.
MODULES ?= java.base,java.logging,jdk.unsupported,java.net.http,jdk.crypto.ec

all: $(OUT)/bin/java

$(OUT)/bin/java:
	test ! -e $(OUT) || rm -r $(OUT)
	$(JLINK) --module-path $(JMODS) --add-modules $(MODULES) \
		--strip-debug --no-header-files --no-man-pages --compress=zip-6 \
		--output $(OUT)

clean:
	test ! -e build || rm -r build

.PHONY: all clean
