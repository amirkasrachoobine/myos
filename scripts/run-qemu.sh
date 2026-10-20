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

for f in bzImage rootfs.ext2; do
    [ -f "$IMAGES/$f" ] || { echo "missing $IMAGES/$f; run 'make' first" >&2; exit 1; }
done

# Use KVM when the host allows it — boots in ~2s instead of ~15s.
ACCEL=(-accel tcg)
if [ -w /dev/kvm ]; then ACCEL=(-accel kvm -cpu host); fi

exec "$QEMU" \
    -M pc "${ACCEL[@]}" -m "${MEM:-512}" -smp "${SMP:-2}" \
    -kernel "$IMAGES/bzImage" \
    -drive file="$IMAGES/rootfs.ext2",if=virtio,format=raw \
    -append "rootwait root=/dev/vda console=ttyS0 ${KERNEL_ARGS:-}" \
    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
    -nographic -no-reboot \
    "$@"
