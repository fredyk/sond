#!/usr/bin/env bash
# Build the Sond release tarball served at sond.dev/release/<channel>/:
#   sond-x86_64-linux.tar.gz          : the whole toolchain in the ~/.local layout
#                       (bin/sondc, bin/sond, lib/sond/, share/sond/Sond.md, share/man/man1/)
#   sond-x86_64-linux.tar.gz.sha256   : its checksum (what install.sh verifies)
#
# Note: the extension is .tar.gz, not .tgz, on purpose: the deploy bundler excludes *.tgz as a
# build artifact, so the tarball must not match that glob to reach the sond.dev container.
#
# Usage: tools/release.sh [out-dir]   (default: dist/release/)
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$REPO/dist/release}"
PLAT="x86_64-linux"
TGZ="sond-$PLAT.tar.gz"

# rustup installs cargo under $CARGO_HOME/bin and puts it on PATH from the shell profile, which a
# non-interactive shell (a deploy script, cron, CI) never sources. Look there before giving up.
if ! command -v cargo >/dev/null 2>&1 && [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]; then
  PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
fi

echo "==> building release binaries (sondc + sond)"
cargo build --release --manifest-path "$REPO/compiler/Cargo.toml"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/lib/sond" "$STAGE/share/sond" "$STAGE/share/man/man1"
install -m 755 "$REPO/compiler/target/release/sondc" "$STAGE/bin/sondc"
install -m 755 "$REPO/compiler/target/release/sond"  "$STAGE/bin/sond"
cp -r "$REPO/lib/." "$STAGE/lib/sond/"

# The man pages ship with the binary. tools/install.sh (building from source) has always
# installed them, so `man sondc` worked there and silently did not for anyone who took the
# one-line installer, which is the path the website recommends first.
install -m 644 "$REPO/docs/man/sondc.1" "$STAGE/share/man/man1/sondc.1"
install -m 644 "$REPO/docs/man/sond.1"  "$STAGE/share/man/man1/sond.1"

echo "==> generating the language reference"
SOND_MD_OUT="$STAGE/share/sond/Sond.md" "$REPO/tools/build-docs.sh"

echo "==> packaging $TGZ (reproducible)"
mkdir -p "$OUT"
# Deterministic archive: same toolchain -> byte-identical tarball -> identical sha256. Fixed sort
# order, mtimes and ownership in tar; `gzip -n` drops the name+timestamp from the gzip header.
# Without this the hash changes every build, so a CDN can serve a stale tarball against a fresh
# checksum and every install fails with "checksum mismatch".
tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
    -C "$STAGE" -cf - bin lib share | gzip -n -9 > "$OUT/$TGZ"
( cd "$OUT" && sha256sum "$TGZ" > "$TGZ.sha256" )
echo "==> done:"
ls -la "$OUT/$TGZ" "$OUT/$TGZ.sha256" | awk '{print "   "$5, $9}'
echo "   sha256: $(cut -d' ' -f1 < "$OUT/$TGZ.sha256")"
