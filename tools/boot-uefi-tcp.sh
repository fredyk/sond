#!/usr/bin/env bash
# Boot a Sond UEFI binary with virtio-net + hostfwd 2222->22, and from the HOST
# side knock on 127.0.0.1:2222 during the boot window so QEMU's slirp opens a TCP
# connection to the guest's port 22. The OS's TCP server must answer the SYN with
# a SYN-ACK. All NIC traffic is dumped to a pcap and the TCP exchange decoded -
# the executable oracle for the TCP milestone (M3) on the road to SSH.
#
# Usage: boot-uefi-tcp.sh <BOOTX64.EFI> [seconds] [pcap]
set -euo pipefail
EFI="${1:?usage: boot-uefi-tcp.sh <BOOTX64.EFI> [seconds] [pcap]}"
SECS="${2:-18}"
PCAP="${3:-$(mktemp -u --suffix=.pcap)}"
OVMF="${SOND_OVMF:-/usr/share/ovmf/OVMF.fd}"
ESP="$(mktemp -d)/esp"; mkdir -p "$ESP/EFI/BOOT"
cp "$EFI" "$ESP/EFI/BOOT/BOOTX64.EFI"

# From the host: knock on the forwarded port a few times so the guest sees SYNs.
# bash's /dev/tcp needs no external tool; each attempt triggers a slirp->guest TCP.
knock() {
  for _ in 1 2 3 4 5 6; do
    sleep 2
    (exec 3<>/dev/tcp/127.0.0.1/2222) 2>/dev/null && { sleep 0.4; exec 3>&- 3<&-; } || true
  done
}
knock &
KNOCKER=$!

echo "sond: booting $EFI with virtio-net + hostfwd 2222->22 (TCP oracle, pcap -> $PCAP)"
timeout "$SECS" qemu-system-x86_64 -bios "$OVMF" \
  -drive format=raw,file=fat:rw:"$ESP",media=disk \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 \
  -device virtio-net-pci,netdev=n0 \
  -object filter-dump,id=dump0,netdev=n0,file="$PCAP" \
  -nographic -m 256 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' \
  | grep -avE 'BdsDxe|Ubuntu 24|Manager|Select Lang|Enter>|option one|Fv\(|PciRoot|Reset|Move Highlight|language for|current system|to change|spice|Continue|GHz|MB RAM|arch_caps|Device Man|Boot Maint|^ *$' \
  || true

kill "$KNOCKER" 2>/dev/null || true
echo "--- TCP exchange on the wire ($PCAP) ---"
if command -v tcpdump >/dev/null 2>&1 && [ -s "$PCAP" ]; then
  tcpdump -nr "$PCAP" 'tcp' 2>/dev/null | head -30
  echo "TCP packets: $(tcpdump -nr "$PCAP" 'tcp' 2>/dev/null | wc -l)"
  echo "SYN-ACK from guest (:22): $(tcpdump -nr "$PCAP" 'tcp[tcpflags] & (tcp-syn|tcp-ack) == (tcp-syn|tcp-ack) and src port 22' 2>/dev/null | wc -l)"
else
  echo "pcap bytes: $(stat -c%s "$PCAP" 2>/dev/null || echo 0)"
fi
