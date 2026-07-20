#!/usr/bin/env bash
# sond-uefi-run <file.snd>: compile to a UEFI app, boot it in QEMU/OVMF headless,
# and print its serial output (OVMF menu noise stripped). The executable oracle for
# UEFI kernels: a worker runs this, reads the output, and iterates until correct.
set -euo pipefail
snd="${1:?usage: sond-uefi-run <file.snd>}"
esp="$(mktemp -d)"; mkdir -p "$esp/EFI/BOOT"
if ! sondc "$snd" --uefi -o "$esp/EFI/BOOT/BOOTX64.EFI" >/dev/null 2>&1; then
  echo "COMPILE-ERROR:"; sondc "$snd" --uefi -o /dev/null 2>&1 | tail -1; rm -rf "$esp"; exit 1
fi
timeout 14 qemu-system-x86_64 -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:"$esp",media=disk -nographic -m 256 -net none 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' \
  | grep -avE 'BdsDxe|Ubuntu 24|Manager|Select Lang|Enter>|option one|Fv\(|PciRoot|Reset|Move Highlight|language for|current system|to change|spice|Continue|GHz|MB RAM|arch_caps|Device Man|Boot Maint|^ *$' \
  || true
rm -rf "$esp"
