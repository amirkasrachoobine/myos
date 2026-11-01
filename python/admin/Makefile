# The myos admin tools (Phase 5): myctl and myos-install, plus the
# myos.admin Python package they share. Pure Python; nothing to build.
#   make -C python/admin install DESTDIR=… SITE=/usr/lib/python3.X/site-packages BINDIR=/usr/bin
# Run from the source tree with PYTHONPATH=python/admin:python/bus.

MODULES := $(wildcard myos/admin/*.py)

all:

install:
	install -d $(DESTDIR)$(SITE)/myos/admin $(DESTDIR)$(BINDIR)
	install -m 644 $(MODULES) $(DESTDIR)$(SITE)/myos/admin/
	install -m 755 myctl myos-install $(DESTDIR)$(BINDIR)/

clean:
	rm -rf $(shell find . -name __pycache__)

.PHONY: all install clean
