#!/usr/bin/env bash
# Build sondc in release and install it to ~/.local/bin/sondc (a canonical binary
# on PATH, so scripts and example-testing use one consistent compiler).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
"$REPO/tools/sandbox.sh" --mem 4G --time 180 -- \
  cargo build --release --manifest-path "$REPO/compiler/Cargo.toml"
mkdir -p "$HOME/.local/bin"
install -m 755 "$REPO/compiler/target/release/sondc" "$HOME/.local/bin/sondc"
# The `sond` toolchain driver (sond test / build / run) alongside the compiler.
install -m 755 "$REPO/compiler/target/release/sond" "$HOME/.local/bin/sond"
# Publish the .snd libraries at a stable system path (a symlink to lib/), so
# isolated programs can `include "any/..."` them without seeing the compiler source.
# COPY (not symlink) the .snd libraries so an isolated worker can include them
# but has NO path back to the compiler source: a symlink would leak the repo dir.
rm -rf "$HOME/.local/lib/sond"
mkdir -p "$HOME/.local/lib/sond"
cp -r "$REPO/lib/." "$HOME/.local/lib/sond/"

# Generate .snd HEADERS (public interface = contract, no bodies) from each library
# into the repo's dist/include/ (a build artifact) and install them to
# ~/.local/include/sond/, the FHS include path (RFC-0006 package interface). A
# consumer will compile against these once separate compilation lands; today the
# source-splice `include` still uses the full lib/ copy above.
DIST="$REPO/dist/include"
rm -rf "$DIST"; mkdir -p "$DIST"
while IFS= read -r src; do
  rel="${src#"$REPO"/lib/}"
  dst="$DIST/$rel"
  mkdir -p "$(dirname "$dst")"
  "$HOME/.local/bin/sondc" "$src" --emit-header > "$dst"
done < <(find "$REPO/lib" -name '*.snd' -type f)
rm -rf "$HOME/.local/include/sond"
mkdir -p "$HOME/.local/include/sond"
cp -r "$DIST/." "$HOME/.local/include/sond/" 2>/dev/null || true

# Generate the compact language reference (never hand-written) into
# ~/.local/share/sond/Sond.md from the marked SOND-MD regions of the design docs.
"$REPO/tools/build-docs.sh"

mkdir -p "$HOME/.local/share/man/man1"
install -m 644 "$REPO/docs/man/sondc.1" "$HOME/.local/share/man/man1/sondc.1"
install -m 644 "$REPO/docs/man/sond.1" "$HOME/.local/share/man/man1/sond.1"

echo "installed: $HOME/.local/bin/sondc"
echo "  libs (source):  $HOME/.local/lib/sond  (copy of lib/)"
echo "  headers:        $HOME/.local/include/sond  (+ repo dist/include/)"
"$HOME/.local/bin/sondc" 2>&1 | head -1 || true
