#!/usr/bin/env bash
# Boot a Sond UEFI application in VirtualBox HEADLESS (VBoxHeadless): BARE UEFI,
# no OS underneath. This is the FINAL validation target: QEMU is used for fast
# iteration during development, VirtualBox for the real end-to-end proof.
#
# It builds a bootable EFI System Partition disk image from the given
# BOOTX64.EFI, wraps it as a VDI, creates/refreshes a VM with EFI firmware and a
# NAT interface that forwards host TCP 2222 -> guest 22 (so an SSH server the OS
# runs is reachable from the host), boots it headless, and tees the guest serial
# to a log file for inspection.
#
# Usage: boot-vbox.sh <BOOTX64.EFI> [vm-name]
#   SOND_VBOX_KEEP=1   leave the VM registered after exit (default: power off)
#   SOND_VBOX_GUI=1    start with a window instead of headless (debugging)
set -euo pipefail

EFI="${1:?usage: boot-vbox.sh <BOOTX64.EFI> [vm-name]}"
VM="${2:-sond-uefi}"
PORT="${SOND_VBOX_PORT:-2222}" # host port forwarded to guest :22 (override to avoid clashes)
# When keeping the VM (KEEP=1) the disk must OUTLIVE the script, so use a stable
# per-VM directory. A mktemp dir deleted by the EXIT trap would pull the VDI out
# from under the still-running VM and leave it stuck at the firmware logo.
if [ "${SOND_VBOX_KEEP:-0}" = 1 ]; then
  WORK="${SOND_VBOX_DIR:-/tmp/sond-vbox-$VM}"; mkdir -p "$WORK"
else
  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
fi
IMG="$WORK/esp.img"
VDI="$WORK/$VM.vdi"
SERIAL="${SOND_VBOX_SERIAL:-$WORK/serial.log}"
rm -f "$VDI"                                            # convertfromraw refuses an existing target
VBoxManage closemedium disk "$VDI" --delete >/dev/null 2>&1 || true

# --- 1. Build the ESP as a FAT32 filesystem image with /EFI/BOOT/BOOTX64.EFI
#     (mtools, no root needed).
ESP_MIB=48
truncate -s "${ESP_MIB}M" "$IMG"
mkfs.vfat -F 32 -n SONDESP "$IMG" >/dev/null
mmd   -i "$IMG" ::/EFI ::/EFI/BOOT
mcopy -i "$IMG" "$EFI" ::/EFI/BOOT/BOOTX64.EFI

# --- 2. Wrap it in a GPT disk with a real EFI System Partition. VirtualBox's EFI
#     firmware (unlike OVMF) will NOT boot a bare-FAT image: it needs a
#     partitioned disk with an ESP (type EF00) for the fallback loader to run.
DISK="$WORK/disk.raw"
first=2048                                   # 1 MiB alignment
esp_sectors=$(( ESP_MIB * 1024 * 1024 / 512 ))
last=$(( first + esp_sectors - 1 ))
truncate -s "$(( (last + 34) * 512 ))" "$DISK"   # + room for the backup GPT
sgdisk -o "$DISK" >/dev/null
sgdisk -n "1:${first}:${last}" -t 1:EF00 -c 1:"EFI System Partition" "$DISK" >/dev/null
dd if="$IMG" of="$DISK" bs=512 seek="$first" conv=notrunc status=none

# --- 3. Wrap the raw GPT disk as a VDI (VirtualBox's native disk format).
VBoxManage convertfromraw "$DISK" "$VDI" --format VDI >/dev/null

# --- 3. Create (or recreate) the VM: EFI firmware, NAT with SSH port-forward.
VBoxManage unregistervm "$VM" --delete >/dev/null 2>&1 || true
VBoxManage createvm --name "$VM" --ostype Other_64 --register >/dev/null
VBoxManage modifyvm "$VM" \
  --firmware efi \
  --memory 256 --cpus 1 \
  --nictype1 virtio \
  --uart1 0x3F8 4 --uartmode1 file "$SERIAL"
# Networking: NAT with an SSH port-forward (default), or a host-only adapter
# (SOND_VBOX_NET=hostonly + SOND_VBOX_HOSTONLY_IF) where the host reaches the guest
# directly by its address on the isolated 192.168.56.0/24 net, no NAT/port-forward.
if [ "${SOND_VBOX_NET:-nat}" = hostonly ]; then
  VBoxManage modifyvm "$VM" --nic1 hostonly --hostonlyadapter1 "${SOND_VBOX_HOSTONLY_IF:-vboxnet0}"
else
  VBoxManage modifyvm "$VM" --nic1 nat
  VBoxManage natnetwork  >/dev/null 2>&1 || true
  VBoxManage modifyvm "$VM" --natpf1 "ssh,tcp,127.0.0.1,$PORT,,22"
fi
# Optional fixed NIC MAC (SOND_VBOX_MAC, 12 hex digits). VirtualBox otherwise assigns
# an 08:00:27:xx address, which the virtio device filters on; set this to match the
# MAC the guest OS uses (e.g. 525400123456, QEMU's default) so unicast is delivered.
if [ -n "${SOND_VBOX_MAC:-}" ]; then
  VBoxManage modifyvm "$VM" --macaddress1 "$SOND_VBOX_MAC"
fi

VBoxManage storagectl "$VM" --name SATA --add sata --controller IntelAhci >/dev/null
VBoxManage storageattach "$VM" --storagectl SATA --port 0 --device 0 \
  --type hdd --medium "$VDI" >/dev/null

# --- 4. Boot. Headless by default; the serial log is the console.
echo "boot-vbox: booting '$VM' on bare UEFI (VirtualBox), serial -> $SERIAL"
echo "boot-vbox: SSH forward host 127.0.0.1:$PORT -> guest :22"
if [ "${SOND_VBOX_GUI:-0}" = 1 ]; then
  VBoxManage startvm "$VM" --type gui
else
  VBoxManage startvm "$VM" --type headless
fi

# Give the guest a moment, then surface the serial output captured so far.
sleep 3
[ -f "$SERIAL" ] && { echo "--- serial ---"; cat "$SERIAL"; echo "--- end serial ---"; }

if [ "${SOND_VBOX_KEEP:-0}" != 1 ]; then
  VBoxManage controlvm "$VM" poweroff >/dev/null 2>&1 || true
  sleep 1
  VBoxManage unregistervm "$VM" --delete >/dev/null 2>&1 || true
fi
