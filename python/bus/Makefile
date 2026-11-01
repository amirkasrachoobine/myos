# Generates myos/bus/v1/*_pb2.py from the shared .proto files.
#   make -C python/bus                    # for running from the source tree
#   make -C python/bus install DESTDIR=… SITE=/usr/lib/python3.X/site-packages BINDIR=/usr/bin
# Buildroot calls the install target with the host protoc in PROTOC.

PROTOC ?= protoc
PROTO_DIR ?= $(abspath ../../proto)
PROTOS := wire bus
GEN := $(PROTOS:%=myos/bus/v1/%_pb2.py)

all: $(GEN) myos/bus/v1/__init__.py

myos/bus/v1/%_pb2.py: $(PROTO_DIR)/myos/bus/v1/%.proto
	@mkdir -p myos/bus/v1
	$(PROTOC) --python_out=. -I$(PROTO_DIR) $<

myos/bus/v1/__init__.py:
	@mkdir -p myos/bus/v1
	touch $@

# `myos` is a namespace package (no __init__.py), so the later Python tools
# can install their own myos.* modules alongside this one.
install: all
	install -d $(DESTDIR)$(SITE)/myos/bus/v1 $(DESTDIR)$(BINDIR)
	install -m 644 myos/bus/__init__.py $(DESTDIR)$(SITE)/myos/bus/
	install -m 644 myos/bus/v1/__init__.py $(GEN) $(DESTDIR)$(SITE)/myos/bus/v1/
	install -m 755 myos-bus-ping-py $(DESTDIR)$(BINDIR)/

clean:
	rm -rf myos/bus/v1 $(shell find . -name __pycache__)

.PHONY: all install clean
