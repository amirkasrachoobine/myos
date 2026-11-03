# Builds myos-sdk.jar: the app SDK (org.myos.sdk) and the launcher jrun
# starts (org.myos.runtime.Main). Plain javac, like java/bus, against
# myos-bus.jar (the bus client and the generated service messages) and
# protobuf-java.
#   make -C java/bus && make -C java/sdk
#   make -C java/sdk install DESTDIR=… JAVADIR=/usr/share/java
# Buildroot passes JAVAC, JAR, BUS_JAR and PROTOBUF_JAR.

JAVAC ?= javac
JAR ?= jar
BUILD ?= build
BUS_JAR ?= ../bus/build/myos-bus.jar
PROTOBUF_JAR ?= ../bus/build/protobuf-java.jar
JAVADIR ?= /usr/share/java
JAVA_RELEASE ?= 17

SRCS := $(shell find src -name '*.java')

all: $(BUILD)/myos-sdk.jar

$(BUILD)/myos-sdk.jar: $(SRCS) $(BUS_JAR) $(PROTOBUF_JAR)
	rm -rf $(BUILD)/classes
	mkdir -p $(BUILD)/classes
	$(JAVAC) --release $(JAVA_RELEASE) -encoding UTF-8 -Xlint:all,-serial,-options,-this-escape \
		-d $(BUILD)/classes -cp $(BUS_JAR):$(PROTOBUF_JAR) $(SRCS)
	$(JAR) --create --file $@ -C $(BUILD)/classes .

# The javadoc, for app writers: make -C java/sdk doc, then build/doc/index.html
doc: $(SRCS)
	javadoc -quiet -d $(BUILD)/doc -cp $(BUS_JAR):$(PROTOBUF_JAR) -sourcepath src -subpackages org.myos.sdk

install: all
	install -d $(DESTDIR)$(JAVADIR)
	install -m 644 $(BUILD)/myos-sdk.jar $(DESTDIR)$(JAVADIR)/myos-sdk.jar

clean:
	rm -rf $(BUILD)

.PHONY: all doc install clean
