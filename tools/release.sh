#!/usr/bin/env bash
# Build the Sond release tarball served at sond.dev/release/<channel>/:
#   sond-x86_64-linux.tar.gz          : the whole toolchain in the ~/.local layout
#                       (bin/sondc, bin/sond, lib/sond/, share/sond/Sond.md, share/man/man1/)
#   sond-x86_64-linux.tar.gz.sha256   : its checksum (what install.sh verifies)
#
# Note: the extension is .tar.gz, not .tgz, on purpose: the deploy bundler excludes *.tgz as a
# build artifact, so the tarball must not match that glob to reach the sond.dev container.
#
# Why TARGET is x86_64-unknown-linux-musl: the released binaries must run on any reasonably
# recent x86-64 Linux, not only on the machine that built them. Building against the musl
# libc yields statically linked executables that bundle their own libc, so a distro whose
# glibc is older than the builder's (Debian 12, Ubuntu 22.04) never sees a
# `version GLIBC_2.3x not found` death on the very first hello world. The alternative
# (building against an old glibc in a container) leaks whichever base image happened to be
# chosen; musl makes the artifact self-contained and is the Rust-blessed route.
# tools/test-release-portable.sh proves this in a fresh debian:12-slim container.
#
# Usage: tools/release.sh [out-dir]   (default: dist/release/)
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$REPO/dist/release}"
PLAT="x86_64-linux"
TGZ="sond-$PLAT.tar.gz"
TARGET="x86_64-unknown-linux-musl"

# rustup installs cargo (and rustup itself) under $CARGO_HOME/bin and puts them on PATH from
# the shell profile, which a non-interactive shell (a deploy script, cron, CI) never sources.
# Look there before giving up.
if ! command -v cargo >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
  CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
  if [ -x "$CARGO_BIN/cargo" ] || [ -x "$CARGO_BIN/rustup" ]; then
    PATH="$CARGO_BIN:$PATH"
  fi
fi

# Hard requirement, checked up front: the static target must be installed. Never fall back to
# the host's dynamic build: that is exactly the broken artifact this script exists to not ship.
if ! command -v rustup >/dev/null 2>&1; then
  echo "error: 'rustup' is required (cargo alone cannot install the $TARGET target std)" >&2
  echo "  install rustup:  curl https://sh.rustup.rs -sSf | sh" >&2
  exit 1
fi
if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "error: the Rust target '$TARGET' is not installed." >&2
  echo "The release must be built statically so it runs on any x86-64 Linux, but it cannot:" >&2
  echo "the musl target is missing. Install it with exactly:" >&2
  echo "  rustup target add $TARGET" >&2
  echo "No tarball was produced." >&2
  exit 1
fi

echo "==> building static release binaries (sondc + sond) for $TARGET"
cargo build --release --target "$TARGET" --manifest-path "$REPO/compiler/Cargo.toml"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/lib/sond" "$STAGE/share/sond" "$STAGE/share/man/man1"
install -m 755 "$REPO/compiler/target/$TARGET/release/sondc" "$STAGE/bin/sondc"
install -m 755 "$REPO/compiler/target/$TARGET/release/sond"  "$STAGE/bin/sond"
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
