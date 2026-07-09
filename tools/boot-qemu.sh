#!/usr/bin/env bash
# Boot a Sond initramfs in QEMU (headless, serial console to stdout).
# Needs a READABLE kernel. Ubuntu's /boot/vmlinuz-* is root-only (0600); make a
# readable copy once:  sudo cp /boot/vmlinuz-$(uname -r) /tmp/sond-vmlinuz && sudo chmod +r /tmp/sond-vmlinuz
# Usage: SOND_KERNEL=/tmp/sond-vmlinuz boot-qemu.sh [initramfs.cpio.gz]
set -euo pipefail
KERNEL="${SOND_KERNEL:-/tmp/sond-vmlinuz}"
INITRD="${1:-/tmp/sond-initramfs.cpio.gz}"
[ -r "$KERNEL" ] || { echo "kernel not readable: $KERNEL, see the header of this script"; exit 1; }
KVM=""; [ -w /dev/kvm ] && KVM="-enable-kvm"
exec qemu-system-x86_64 $KVM \
  -kernel "$KERNEL" -initrd "$INITRD" \
  -append 'console=ttyS0 panic=1' \
  -nographic -m 256 -no-reboot
