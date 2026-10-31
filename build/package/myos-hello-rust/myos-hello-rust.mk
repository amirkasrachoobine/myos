################################################################################
#
# myos-hello-rust
#
################################################################################

MYOS_HELLO_RUST_VERSION = 0.1.0
MYOS_HELLO_RUST_SITE = $(BR2_EXTERNAL_MYOS_PATH)/../rust/hello
MYOS_HELLO_RUST_SITE_METHOD = local
MYOS_HELLO_RUST_LICENSE = MIT

$(eval $(cargo-package))
