#!/usr/bin/env bash
# Boot a Sond UEFI binary with virtio-net + hostfwd 2222->22, then run the REAL
# OpenSSH client against it and capture the -vvv trace. The oracle for the SSH
# transport milestones beyond the banner: it shows how far a genuine SSH client
# gets: banner exchange, SSH2_MSG_KEXINIT, key exchange, and so on.
#
# Usage: boot-uefi-sshclient.sh <BOOTX64.EFI> [seconds]
set -euo pipefail
EFI="${1:?usage: boot-uefi-sshclient.sh <BOOTX64.EFI> [seconds]}"
SECS="${2:-22}"
OVMF="${SOND_OVMF:-/usr/share/ovmf/OVMF.fd}"
ESP="$(mktemp -d)/esp"; mkdir -p "$ESP/EFI/BOOT"
cp "$EFI" "$ESP/EFI/BOOT/BOOTX64.EFI"
OUT="$(mktemp)"

run_ssh() {
  sleep 5
  timeout 12 ssh -vvv -p 2222 \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=8 -o PreferredAuthentications=none \
    -o KexAlgorithms=+curve25519-sha256 \
    tester@127.0.0.1 true 2>&1 \
    | grep -iE 'remote software version|SSH2_MSG_KEXINIT|kex: |sending SSH2_MSG_KEXINIT|no matching|Connection|banner|expecting SSH2' \
    > "$OUT" || true
}
run_ssh &
SSHPID=$!

PCAP="$(mktemp -u --suffix=.pcap)"
echo "sond: booting $EFI + real OpenSSH client (-vvv) to :2222"
timeout "$SECS" qemu-system-x86_64 -bios "$OVMF" \
  -drive format=raw,file=fat:rw:"$ESP",media=disk \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 \
  -device virtio-net-pci,netdev=n0 \
  -object filter-dump,id=d0,netdev=n0,file="$PCAP" \
  -nographic -m 256 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' \
  | grep -avE 'BdsDxe|Ubuntu 24|Manager|Select Lang|Enter>|option one|Fv\(|PciRoot|Reset|Move Highlight|language for|current system|to change|spice|Continue|GHz|MB RAM|arch_caps|Device Man|Boot Maint|^ *$' \
  || true

kill "$SSHPID" 2>/dev/null || true
if command -v tcpdump >/dev/null 2>&1 && [ -s "$PCAP" ]; then
  echo "--- TCP on the wire (check that your reply's dest port = the client's src port) ---"
  tcpdump -nr "$PCAP" 'tcp' 2>/dev/null | head -8
fi
rm -f "$PCAP"
echo "--- OpenSSH client trace ---"
if [ -s "$OUT" ]; then
  cat "$OUT"
  grep -qi 'remote software version Sond' "$OUT" && echo "SSH-BANNER-OK"
  grep -qi 'SSH2_MSG_KEXINIT received' "$OUT" && echo "SSH-KEXINIT-OK"
else
  echo "(no SSH client output)"
fi
rm -f "$OUT"
