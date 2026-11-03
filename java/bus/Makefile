# Builds myos-bus.jar (generated protobuf classes for the bus, log, net and
# sysinfo, the client, and BusService for serving a name) with plain
# javac, no Maven/Gradle. Needs protobuf-java at a version matching protoc:
#   make -C java/bus fetch-protobuf     # downloads it from Maven Central
#   make -C java/bus
#   make -C java/bus install DESTDIR=… JAVADIR=/usr/share/java BINDIR=/usr/bin
# Buildroot passes PROTOC, JAVAC, JAR and PROTOBUF_JAR.

PROTOC ?= protoc
JAVAC ?= javac
JAR ?= jar
PROTO_DIR ?= $(abspath ../../proto)
BUILD ?= build
PROTOBUF_JAR ?= $(BUILD)/protobuf-java.jar
JAVADIR ?= /usr/share/java
BINDIR ?= /usr/bin
# Unix-domain SocketChannel needs Java 16+; 17 is the oldest LTS that has it.
JAVA_RELEASE ?= 17

# The bus itself, and (Phase 6, for the app SDK) the core services' messages.
PROTOS := $(PROTO_DIR)/myos/bus/v1/wire.proto $(PROTO_DIR)/myos/bus/v1/bus.proto \
	$(PROTO_DIR)/myos/log/v1/log.proto $(PROTO_DIR)/myos/net/v1/net.proto \
	$(PROTO_DIR)/myos/sysinfo/v1/sysinfo.proto
SRCS := $(shell find src -name '*.java')

all: $(BUILD)/myos-bus.jar

$(BUILD)/myos-bus.jar: $(PROTOS) $(SRCS) $(PROTOBUF_JAR)
	rm -rf $(BUILD)/gen $(BUILD)/classes
	mkdir -p $(BUILD)/gen $(BUILD)/classes
	$(PROTOC) --java_out=$(BUILD)/gen -I$(PROTO_DIR) $(PROTOS)
	$(JAVAC) --release $(JAVA_RELEASE) -encoding UTF-8 -nowarn -d $(BUILD)/classes -cp $(PROTOBUF_JAR) \
		$(SRCS) $$(find $(BUILD)/gen -name '*.java')
	$(JAR) --create --file $@ -C $(BUILD)/classes .

# protoc 3.x pairs with protobuf-java 3.x; protoc 22-25 with 3.22-3.25;
# protoc 26 and later (e.g. 33.5) with 4.26 and later (4.33.5).
PROTOC_VERSION = $(shell $(PROTOC) --version | awk '{print $$2}')
# (The "(pattern)" case syntax keeps make's $(shell ...) parentheses balanced.)
PROTOBUF_JAVA_VERSION = $(shell v=$(PROTOC_VERSION); case $$v in (3.*) echo $$v;; (*) m=$${v%%.*}; if [ $$m -ge 26 ]; then echo 4.$$v; else echo 3.$$v; fi;; esac)

fetch-protobuf:
	mkdir -p $(BUILD)
	curl -fL -o $(BUILD)/protobuf-java.jar \
		https://repo1.maven.org/maven2/com/google/protobuf/protobuf-java/$(PROTOBUF_JAVA_VERSION)/protobuf-java-$(PROTOBUF_JAVA_VERSION).jar

# Or build protobuf-java from a protobuf source tree, as Buildroot does from
# the tarball it already downloaded and hash-checked for host-protobuf.
# PROTOBUF_SRC must be the same release as $(PROTOC). The core runtime has
# no dependencies beyond the JDK: generate the well-known types, compile.
WKT := any api descriptor duration empty field_mask source_context struct timestamp type wrappers compiler/plugin
protobuf-from-source:
	@test -d "$(PROTOBUF_SRC)/java/core/src/main/java" || \
		{ echo "set PROTOBUF_SRC to an unpacked protobuf release matching $(PROTOC_VERSION)" >&2; exit 1; }
	rm -rf $(BUILD)/pbj
	mkdir -p $(BUILD)/pbj/gen $(BUILD)/pbj/classes
	$(PROTOC) --java_out=$(BUILD)/pbj/gen -I$(PROTOBUF_SRC)/src -I$(PROTOBUF_SRC)/java/core/src/main/resources \
		$(PROTOBUF_SRC)/java/core/src/main/resources/google/protobuf/java_features.proto \
		$(WKT:%=$(PROTOBUF_SRC)/src/google/protobuf/%.proto)
	$(JAVAC) --release 11 -nowarn -encoding UTF-8 -d $(BUILD)/pbj/classes \
		$$(find $(PROTOBUF_SRC)/java/core/src/main/java $(BUILD)/pbj/gen -name '*.java')
	cp -r $(PROTOBUF_SRC)/java/core/src/main/resources/google $(BUILD)/pbj/classes/
	$(JAR) --create --file $(BUILD)/protobuf-java.jar -C $(BUILD)/pbj/classes .
	rm -rf $(BUILD)/pbj

$(BUILD)/protobuf-java.jar:
	@echo "missing $@: run 'make fetch-protobuf', 'make protobuf-from-source PROTOBUF_SRC=…'," >&2
	@echo "or pass PROTOBUF_JAR=/path/to/protobuf-java.jar" >&2; exit 1

install: all
	install -d $(DESTDIR)$(JAVADIR) $(DESTDIR)$(BINDIR)
	install -m 644 $(BUILD)/myos-bus.jar $(DESTDIR)$(JAVADIR)/myos-bus.jar
	install -m 644 $(PROTOBUF_JAR) $(DESTDIR)$(JAVADIR)/protobuf-java.jar
	printf '#!/bin/sh\nexec java -cp %s/myos-bus.jar:%s/protobuf-java.jar org.myos.bus.Ping "$$@"\n' \
		$(JAVADIR) $(JAVADIR) > $(DESTDIR)$(BINDIR)/myos-bus-ping-java
	chmod 755 $(DESTDIR)$(BINDIR)/myos-bus-ping-java

clean:
	rm -rf $(BUILD)

.PHONY: all install clean fetch-protobuf protobuf-from-source
