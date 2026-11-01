#!/usr/bin/env bash
# Boot the myos image in QEMU with the serial console on this terminal.
# Exit QEMU with: Ctrl-A then X.   Login: root / myos
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
O="${O:-$ROOT/output}"
IMAGES="$O/images"

# Prefer the QEMU Buildroot built for us; fall back to the system one.
QEMU="$O/host/bin/qemu-system-x86_64"
[ -x "$QEMU" ] || QEMU="$(command -v qemu-system-x86_64 || true)"
[ -n "$QEMU" ] || { echo "qemu-system-x86_64 not found; run 'make' first" >&2; exit 1; }

# Default: load the kernel straight from output/images and use rootfs.ext2
# as the disk. DISK=FILE instead boots FILE through its own bootloader, as a
# PC would: a disk myos-install wrote (Phase 5).
# EXTRA_DISK=FILE attaches FILE as a second disk (/dev/vdb), to install onto.
if [ -n "${DISK:-}" ]; then
    [ -f "$DISK" ] || { echo "DISK=$DISK: no such file" >&2; exit 1; }
    BOOT=(-drive file="$DISK",if=virtio,format=raw)
else
    for f in bzImage rootfs.ext2; do
        [ -f "$IMAGES/$f" ] || { echo "missing $IMAGES/$f; run 'make' first" >&2; exit 1; }
    done
    BOOT=(-kernel "$IMAGES/bzImage"
          -drive file="$IMAGES/rootfs.ext2",if=virtio,format=raw
          -append "rootwait root=/dev/vda console=ttyS0 ${KERNEL_ARGS:-}")
fi
EXTRA=()
if [ -n "${EXTRA_DISK:-}" ]; then
    [ -f "$EXTRA_DISK" ] || { echo "EXTRA_DISK=$EXTRA_DISK: no such file (truncate -s 2G $EXTRA_DISK)" >&2; exit 1; }
    EXTRA=(-drive file="$EXTRA_DISK",if=virtio,format=raw)
fi

# Use KVM when the host allows it — boots in ~2s instead of ~15s.
ACCEL=(-accel tcg)
if [ -w /dev/kvm ]; then ACCEL=(-accel kvm -cpu host); fi

exec "$QEMU" \
    -M pc "${ACCEL[@]}" -m "${MEM:-512}" -smp "${SMP:-2}" \
    "${BOOT[@]}" "${EXTRA[@]}" \
    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
    -nographic -no-reboot \
    "$@"
