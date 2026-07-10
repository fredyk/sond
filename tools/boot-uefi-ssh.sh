#!/usr/bin/env bash
# Boot a Sond UEFI binary with virtio-net + hostfwd 2222->22, then from the HOST
# open a TCP connection to 127.0.0.1:2222, send an SSH client identification
# banner, and read back whatever the OS's SSH server sends. The executable oracle
# for the SSH transport milestones: the OS must complete the TCP handshake AND
# send its "SSH-2.0-..." banner as TCP data (proving TCP data transfer works too).
#
# Usage: boot-uefi-ssh.sh <BOOTX64.EFI> [seconds]
set -euo pipefail
EFI="${1:?usage: boot-uefi-ssh.sh <BOOTX64.EFI> [seconds]}"
SECS="${2:-20}"
OVMF="${SOND_OVMF:-/usr/share/ovmf/OVMF.fd}"
ESP="$(mktemp -d)/esp"; mkdir -p "$ESP/EFI/BOOT"
cp "$EFI" "$ESP/EFI/BOOT/BOOTX64.EFI"
OUT="$(mktemp)"

# From the host: after the OS has booted, speak SSH client-side. Send our banner
# and capture the server's response. Retry a few times during the boot window.
speak_ssh() {
  for _ in 1 2 3 4 5; do
    sleep 3
    resp="$(printf 'SSH-2.0-SondOracle_1.0\r\n' \
      | timeout 3 nc -w2 127.0.0.1 2222 2>/dev/null | tr -d '\000')"
    if [ -n "$resp" ]; then
      printf 'SERVER-SENT: %s\n' "$resp" >> "$OUT"
      break
    fi
  done
}
speak_ssh &
SPEAKER=$!

echo "sond: booting $EFI (SSH oracle: host sends SSH banner to :2222 -> guest :22)"
timeout "$SECS" qemu-system-x86_64 -bios "$OVMF" \
  -drive format=raw,file=fat:rw:"$ESP",media=disk \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 \
  -device virtio-net-pci,netdev=n0 \
  -nographic -m 256 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' \
  | grep -avE 'BdsDxe|Ubuntu 24|Manager|Select Lang|Enter>|option one|Fv\(|PciRoot|Reset|Move Highlight|language for|current system|to change|spice|Continue|GHz|MB RAM|arch_caps|Device Man|Boot Maint|^ *$' \
  || true

kill "$SPEAKER" 2>/dev/null || true
echo "--- SSH exchange (host side) ---"
if [ -s "$OUT" ]; then
  cat "$OUT"
  grep -q 'SERVER-SENT: SSH-2.0' "$OUT" && echo "SSH-BANNER-OK" || echo "SSH-BANNER-MISSING"
else
  echo "SSH-BANNER-MISSING (no data from server)"
fi
rm -f "$OUT"
