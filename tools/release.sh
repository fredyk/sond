#!/usr/bin/env bash
# Build the Sond release tarball served at sond.dev/release/<channel>/:
#   sond-x86_64-linux.tgz         , the whole toolchain in the ~/.local layout
#                                    (bin/sondc, bin/sond, lib/sond/, share/sond/Sond.md)
#   sond-x86_64-linux.tgz.sha256  , its checksum (what install.sh verifies)
#
# Usage: tools/release.sh [out-dir]   (default: dist/release/)
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$REPO/dist/release}"
PLAT="x86_64-linux"
TGZ="sond-$PLAT.tgz"

echo "==> building release binaries (sondc + sond)"
cargo build --release --manifest-path "$REPO/compiler/Cargo.toml"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/lib/sond" "$STAGE/share/sond"
install -m 755 "$REPO/compiler/target/release/sondc" "$STAGE/bin/sondc"
install -m 755 "$REPO/compiler/target/release/sond"  "$STAGE/bin/sond"
cp -r "$REPO/lib/." "$STAGE/lib/sond/"

echo "==> generating the language reference"
SOND_MD_OUT="$STAGE/share/sond/Sond.md" "$REPO/tools/build-docs.sh"

echo "==> packaging $TGZ"
mkdir -p "$OUT"
tar -czf "$OUT/$TGZ" -C "$STAGE" bin lib share
( cd "$OUT" && sha256sum "$TGZ" > "$TGZ.sha256" )
echo "==> done:"
ls -la "$OUT/$TGZ" "$OUT/$TGZ.sha256" | awk '{print "   "$5, $9}'
echo "   sha256: $(cut -d' ' -f1 < "$OUT/$TGZ.sha256")"
