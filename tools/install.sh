#!/usr/bin/env bash
# Build sondc in release and install it to ~/.local/bin/sondc (a canonical binary
# on PATH, so scripts and example-testing use one consistent compiler).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
# rustup installs cargo under $CARGO_HOME/bin and puts it on PATH from the shell profile, which a
# non-interactive shell (a deploy script, cron, CI) never sources. Look there before giving up.
if ! command -v cargo >/dev/null 2>&1 && [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]; then
  PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
fi

"$REPO/tools/sandbox.sh" --mem 4G --time 180 -- \
  cargo build --release --manifest-path "$REPO/compiler/Cargo.toml"
# Every destination below is a path something else is reading right now -- another shell's
# `sondc`, a worker resolving `include "any/..."`, a running build. So none of them is emptied
# and refilled in place: each is staged beside itself and swapped in by rename, which is why the
# copies go through publish-file.sh / publish-tree.sh instead of `install` and `cp -r`.
PUBFILE="$REPO/tools/publish-file.sh"
PUBTREE="$REPO/tools/publish-tree.sh"

# Where that build went: cargo decides (a `build.target-dir` or CARGO_TARGET_DIR moves it).
RELEASE="$("$REPO/tools/release-dir.sh")"

mkdir -p "$HOME/.local/bin"
"$PUBFILE" "$RELEASE/sondc" "$HOME/.local/bin/sondc" 755
# The `sond` toolchain driver (sond test / build / run) alongside the compiler.
"$PUBFILE" "$RELEASE/sond" "$HOME/.local/bin/sond" 755
# Publish the .snd libraries at a stable system path so isolated programs can
# `include "any/..."` them without seeing the compiler source.
# COPY (not symlink to the repo) the .snd libraries so an isolated worker can include them
# but has NO path back to the compiler source: a link into lib/ would leak the repo dir.
"$PUBTREE" "$REPO/lib" "$HOME/.local/lib/sond"

# Generate .snd HEADERS (public interface = contract, no bodies) from each library
# into the repo's dist/include/ (a build artifact) and install them to
# ~/.local/include/sond/, the FHS include path (RFC-0006 package interface). A
# consumer will compile against these once separate compilation lands; today the
# source-splice `include` still uses the full lib/ copy above.
DIST="$REPO/dist/include"
mkdir -p "$REPO/dist"
# The headers are generated into a scratch tree first and published whole. Generating straight
# into $DIST would leave it half-regenerated for the length of the loop, and a header that a
# concurrent build spliced in mid-loop is a compile error with no cause anywhere near it.
GEN="$(mktemp -d "$REPO/dist/.include.gen.XXXXXX")"
trap 'rm -rf "$GEN"' EXIT
while IFS= read -r src; do
  rel="${src#"$REPO"/lib/}"
  dst="$GEN/$rel"
  mkdir -p "$(dirname "$dst")"
  "$HOME/.local/bin/sondc" "$src" --emit-header > "$dst"
done < <(find "$REPO/lib" -name '*.snd' -type f)
# --as-directory: git tracks files under dist/include, so that one stays a real directory.
"$PUBTREE" --as-directory "$GEN" "$DIST"
"$PUBTREE" "$GEN" "$HOME/.local/include/sond"

# Generate the compact language reference (never hand-written) into
# ~/.local/share/sond/Sond.md from the marked SOND-MD regions of the design docs.
"$REPO/tools/build-docs.sh"

mkdir -p "$HOME/.local/share/man/man1"
"$PUBFILE" "$REPO/docs/man/sondc.1" "$HOME/.local/share/man/man1/sondc.1" 644
"$PUBFILE" "$REPO/docs/man/sond.1" "$HOME/.local/share/man/man1/sond.1" 644

echo "installed: $HOME/.local/bin/sondc"
echo "  libs (source):  $HOME/.local/lib/sond  (copy of lib/)"
echo "  headers:        $HOME/.local/include/sond  (+ repo dist/include/)"
"$HOME/.local/bin/sondc" 2>&1 | head -1 || true
