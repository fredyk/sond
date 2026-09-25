#!/usr/bin/env bash
# Verify the kernel under lib/x86_64/linux/kernel/.
#
# This gate is IN ADDITION to the coverage gate, never instead of it. A driver that reaches 80%
# coverage can still fail to bring the machine up, and a kernel that boots can still be full of
# untested logic. Both questions have to be asked, because they are different questions.
#
# Booting is the one thing a kernel cannot fake: the image either passes ExitBootServices, sets up
# the GDT, the IDT, the PIC and the timer, and talks to COM1, or it does not.
#
# Where a driver touches hardware that a hosted test cannot reach (printk writing to port 0x3F8,
# i8042 reading the keyboard controller), the answer is to take the port doubles from
# lib/x86_64/linux/mocks.snd and pass them in, not to excuse the file from being tested. "It
# cannot be tested" almost always means a seam is missing.
#
#   tools/test-kernel.sh
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL="$REPO/lib/x86_64/linux/kernel/6.18"
SONDC="${SONDC:-$HOME/.local/bin/sondc}"
[ -x "$SONDC" ] || SONDC="$("$REPO/tools/release-dir.sh")/sondc"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fail=0

# --- 1. The unit-testable parts, with the normal coverage gate -----------------
echo "==> unit tests"
while IFS= read -r t; do
    [ -n "$t" ] || continue
    name="${t#"$KERNEL"/}"
    if "$SONDC" "$t" --test -o "$WORK/t" >/dev/null 2>&1; then
        if out="$(timeout 60 "$WORK/t" 2>&1)"; then
            printf '    %-46s %s\n' "$name" "$(printf '%s' "$out" | tail -1)"
        else
            printf '    %-46s DID NOT TERMINATE\n' "$name"; fail=1
        fi
    else
        printf '    %-46s DID NOT BUILD\n' "$name"; fail=1
    fi
done < <(cd "$REPO" && git ls-files "lib/x86_64/linux/kernel/*_test.snd" | sed "s|^|$REPO/|")

# --- 2. The boot gate ----------------------------------------------------------
# Built by hand rather than with QEMU's `fat:rw:` helper, which writes a scratch image under
# /var and fails outright when that filesystem is full.
echo "==> boot"
for tool in qemu-system-x86_64 mkfs.vfat mmd mcopy; do
    command -v "$tool" >/dev/null || { echo "    skipped: $tool is not installed"; exit "$fail"; }
done
OVMF=""
for c in /usr/share/ovmf/OVMF.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/edk2-ovmf/x64/OVMF.fd; do
    [ -f "$c" ] && { OVMF="$c"; break; }
done
[ -n "$OVMF" ] || { echo "    skipped: no OVMF firmware found"; exit "$fail"; }

"$SONDC" "$KERNEL/init/main.snd" --uefi -o "$WORK/kernel.efi" >/dev/null
dd if=/dev/zero of="$WORK/esp.img" bs=1M count=48 status=none
mkfs.vfat "$WORK/esp.img" >/dev/null
mmd -i "$WORK/esp.img" ::/EFI ::/EFI/BOOT
mcopy -i "$WORK/esp.img" "$WORK/kernel.efi" ::/EFI/BOOT/BOOTX64.EFI

# No KVM here, so this is TCG and takes about half a minute to reach the banner.
timeout 90 qemu-system-x86_64 \
    -bios "$OVMF" \
    -drive "format=raw,file=$WORK/esp.img,media=disk" \
    -serial "file:$WORK/serial.log" \
    -display none -m 256 -net none < /dev/null > /dev/null 2>&1 || true

# Every stage has to report in. A kernel that boots and then says nothing is not booting.
expected=(
    "Exiting boot services"
    "GDT loaded"
    "IDT loaded"
    "PIC remapped"
    "PIT initialized"
    "Interrupts enabled"
    "booted"
)
for line in "${expected[@]}"; do
    if grep -qF "$line" "$WORK/serial.log" 2>/dev/null; then
        printf '    ok      %s\n' "$line"
    else
        printf '    MISSING %s\n' "$line"; fail=1
    fi
done

echo
if [ "$fail" -eq 0 ]; then
    echo "kernel: unit tests pass and the image boots to its idle loop"
else
    echo "kernel: FAILED"
    echo "--- serial output ---"
    cat "$WORK/serial.log" 2>/dev/null || echo "(no serial output at all)"
    exit 1
fi
