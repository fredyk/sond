#!/usr/bin/env bash
# Boot a Sond UEFI binary in QEMU with a REAL virtio-net NIC, the fast (dev)
# counterpart to boot-vbox.sh for the networking milestones (M1/M2 on the road
# to SSH). Same NIC type (virtio) as VirtualBox so the driver is identical.
#
# - NAT/user networking with host 2222 -> guest 22 forwarded (the OS's SSH port).
# - Every frame on the NIC is dumped to a pcap so you can PROVE the OS put a
#   packet on the wire (M1) without any guest cooperation.
#
# Usage: boot-uefi-net.sh <BOOTX64.EFI> [seconds] [pcap-out]
set -euo pipefail
EFI="${1:?usage: boot-uefi-net.sh <BOOTX64.EFI> [seconds] [pcap-out]}"
SECS="${2:-12}"
PCAP="${3:-$(mktemp -u --suffix=.pcap)}"
OVMF="${SOND_OVMF:-/usr/share/ovmf/OVMF.fd}"
ESP="$(mktemp -d)/esp"; mkdir -p "$ESP/EFI/BOOT"
cp "$EFI" "$ESP/EFI/BOOT/BOOTX64.EFI"

echo "sond: booting $EFI with virtio-net (pcap -> $PCAP, host :2222 -> guest :22)"
timeout "$SECS" qemu-system-x86_64 -bios "$OVMF" \
  -drive format=raw,file=fat:rw:"$ESP",media=disk \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 \
  -device virtio-net-pci,netdev=n0 \
  -object filter-dump,id=dump0,netdev=n0,file="$PCAP" \
  -nographic -m 256 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' \
  | grep -avE 'BdsDxe|Ubuntu 24|Manager|Select Lang|Enter>|option one|Fv\(|PciRoot|Reset|Move Highlight|language for|current system|to change|spice|Continue|GHz|MB RAM|arch_caps|Device Man|Boot Maint|^ *$' \
  || true

echo "--- NIC capture ($PCAP) ---"
if command -v tcpdump >/dev/null 2>&1 && [ -s "$PCAP" ]; then
  tcpdump -nr "$PCAP" 2>/dev/null | head -40
  echo "frames captured: $(tcpdump -nr "$PCAP" 2>/dev/null | wc -l)"
else
  echo "pcap bytes: $(stat -c%s "$PCAP" 2>/dev/null || echo 0) (install tcpdump to decode)"
fi
