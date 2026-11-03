# The sample app: hello.jar, packaged with app.toml as org.myos.hello.mapp,
# an uncompressed ustar archive that `jrun install` takes.
#   make -C java/bus && make -C java/sdk && make -C java/apps/hello
#   make -C java/apps/hello install DESTDIR=… APPDIR=/usr/share/myos/apps
# Buildroot passes JAVAC, JAR, TAR, SDK_JAR, BUS_JAR and PROTOBUF_JAR.

JAVAC ?= javac
JAR ?= jar
TAR ?= tar
BUILD ?= build
SDK_JAR ?= ../../sdk/build/myos-sdk.jar
BUS_JAR ?= ../../bus/build/myos-bus.jar
PROTOBUF_JAR ?= ../../bus/build/protobuf-java.jar
APPDIR ?= /usr/share/myos/apps
JAVA_RELEASE ?= 17
PACKAGE := org.myos.hello.mapp

SRCS := $(shell find src -name '*.java')

all: $(BUILD)/$(PACKAGE)

$(BUILD)/hello.jar: $(SRCS) $(SDK_JAR) $(BUS_JAR) $(PROTOBUF_JAR)
	test ! -e $(BUILD)/classes || rm -r $(BUILD)/classes
	mkdir -p $(BUILD)/classes
	$(JAVAC) --release $(JAVA_RELEASE) -encoding UTF-8 -Xlint:all,-serial,-options,-this-escape \
		-d $(BUILD)/classes -cp $(SDK_JAR):$(BUS_JAR):$(PROTOBUF_JAR) $(SRCS)
	$(JAR) --create --file $@ -C $(BUILD)/classes .

# Same bytes every build: fixed owner, time and order.
$(BUILD)/$(PACKAGE): $(BUILD)/hello.jar app.toml
	test ! -e $(BUILD)/pkg || rm -r $(BUILD)/pkg
	mkdir -p $(BUILD)/pkg
	cp app.toml $(BUILD)/hello.jar $(BUILD)/pkg/
	$(TAR) --format=ustar --owner=0 --group=0 --numeric-owner --mtime=@0 --sort=name \
		--mode=u=rwX,go=rX -cf $@ -C $(BUILD)/pkg app.toml hello.jar

install: all
	install -d $(DESTDIR)$(APPDIR)
	install -m 644 $(BUILD)/$(PACKAGE) $(DESTDIR)$(APPDIR)/$(PACKAGE)

clean:
	test ! -e $(BUILD) || rm -r $(BUILD)

.PHONY: all install clean
