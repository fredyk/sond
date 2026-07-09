#!/usr/bin/env bash
# Boot a Sond UEFI application in QEMU via OVMF firmware, BARE UEFI, no OS.
# The firmware loads /EFI/BOOT/BOOTX64.EFI directly. Serial console to stdout.
# Usage: boot-uefi.sh <BOOTX64.EFI>
set -euo pipefail
EFI="${1:?usage: boot-uefi.sh <BOOTX64.EFI>}"
OVMF="${SOND_OVMF:-/usr/share/ovmf/OVMF.fd}"
ESP="$(mktemp -d)/esp"; mkdir -p "$ESP/EFI/BOOT"
cp "$EFI" "$ESP/EFI/BOOT/BOOTX64.EFI"
KVM=""; [ -w /dev/kvm ] && KVM="-enable-kvm"
echo "sond: booting $EFI on bare UEFI (OVMF)..."
exec qemu-system-x86_64 $KVM -bios "$OVMF" \
  -drive format=raw,file=fat:rw:"$ESP",media=disk \
  -nographic -m 256 -net none
