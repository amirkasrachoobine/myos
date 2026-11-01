# Generates the Python protobuf modules from the shared .proto files:
#   myos/bus/v1/{wire,bus}_pb2.py     the bus itself (Phase 2)
#   myos/log/v1/log_pb2.py            logd's "log" service (Phase 5)
#   myos/net/v1/net_pb2.py            netd's "net" service
#   myos/sysinfo/v1/sysinfo_pb2.py    sysinfod's "sysinfo" service
#
#   make -C python/bus                    # for running from the source tree
#   make -C python/bus install DESTDIR=… SITE=/usr/lib/python3.X/site-packages BINDIR=/usr/bin
# Buildroot calls the install target with the host protoc in PROTOC.

PROTOC ?= protoc
PROTO_DIR ?= $(abspath ../../proto)
BUS_GEN := myos/bus/v1/wire_pb2.py myos/bus/v1/bus_pb2.py
SVC_GEN := myos/log/v1/log_pb2.py myos/net/v1/net_pb2.py myos/sysinfo/v1/sysinfo_pb2.py
GEN := $(BUS_GEN) $(SVC_GEN)
INITS := $(sort $(foreach f,$(GEN),$(dir $(f))__init__.py))

all: $(GEN) $(INITS)

# One pattern for every package: myos/<pkg>/v1/<name>_pb2.py from the
# .proto at the same path under $(PROTO_DIR).
myos/%_pb2.py: $(PROTO_DIR)/myos/%.proto
	@mkdir -p $(dir $@)
	$(PROTOC) --python_out=. -I$(PROTO_DIR) $<

myos/%/v1/__init__.py:
	@mkdir -p $(dir $@)
	touch $@

# `myos` is a namespace package (no __init__.py), and so are myos.log,
# myos.net and myos.sysinfo, so other Python tools (python/admin) can
# install their own myos.* modules alongside these.
install: all
	install -d $(DESTDIR)$(BINDIR)
	install -D -m 644 myos/bus/__init__.py $(DESTDIR)$(SITE)/myos/bus/__init__.py
	for f in $(GEN) $(INITS); do install -D -m 644 $$f $(DESTDIR)$(SITE)/$$f || exit 1; done
	install -m 755 myos-bus-ping-py $(DESTDIR)$(BINDIR)/

clean:
	rm -rf myos/bus/v1 myos/log myos/net myos/sysinfo $(shell find . -name __pycache__)

.PHONY: all install clean
