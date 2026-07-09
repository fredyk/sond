#!/usr/bin/env bash
# Package a compiled freestanding Sond binary as an initramfs /init (+ a
# /dev/console node so the init's write() reaches the serial console).
# Usage: build-initramfs.sh <sond-init-binary> [out.cpio.gz]
set -euo pipefail
BIN="${1:?usage: build-initramfs.sh <init-binary> [out.cpio.gz]}"
OUT="${2:-/tmp/sond-initramfs.cpio.gz}"
ROOT="$(mktemp -d)"
cp "$BIN" "$ROOT/init"; chmod +x "$ROOT/init"
mkdir -p "$ROOT/dev"
# /dev/console is needed for the init's stdout to reach the console. Creating a
# device node needs root; if unavailable, the kernel still runs init but write()
# to fd 1 may not appear. Try mknod, ignore failure.
mknod "$ROOT/dev/console" c 5 1 2>/dev/null || true
( cd "$ROOT" && find . -print0 | cpio --null -o -H newc 2>/dev/null | gzip ) > "$OUT"
rm -rf "$ROOT"
echo "sond: wrote initramfs $OUT ($(wc -c < "$OUT") bytes)"
