#!/usr/bin/env bash
# Prove that the release tarball actually works on a stock modern Linux, not only on the
# machine that built it.
#
# The failure this guards against: the release tarball shipped binaries linked against the
# builder's glibc. On any distribution with an older glibc (Debian 12 is glibc 2.36), the
# install looked perfect and `sondc` then died on the very first hello world:
#
#     sondc: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found (required by sondc)
#
# The check has two layers:
#   1. every released binary (`sondc`, `sond`) must be a STATICALLY linked ELF, and
#   2. the whole flow reruns inside a `debian:12-slim` container: install the tarball the
#      way the one-line installer does, compile a hello world there, and require it to print
#      the expected text and exit with the exact expected code.
#
# Requires: docker (rootless, used as-is, never `sudo docker`), `file`, and a network for the
# container to `apt-get` a C compiler, because `sondc` always links its output through `cc`.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="$REPO/dist/release"
TGZ="sond-x86_64-linux.tar.gz"
TARBALL="$TARGET_DIR/$TGZ"
IMAGE="debian:12-slim"
EXPECT=42

fail() {
    echo "PORTABLE-RELEASE TEST FAILED: $1" >&2
    exit 1
}

echo "==> [1/4] building the release tarball"
"$REPO/tools/release.sh" "$TARGET_DIR"
[ -f "$TARBALL" ] || fail "no tarball produced at $TARBALL"

echo "==> [2/4] released binaries must be statically linked (no dependency on the builder's glibc)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
tar -xzf "$TARBALL" -C "$STAGE" bin/sondc bin/sond
for bin in sondc sond; do
    desc="$(file -b -- "$STAGE/bin/$bin")"
    printf '   bin/%s: %s\n' "$bin" "$(printf '%s' "$desc" | cut -c1-78)"
    case "$desc" in
        # `file` spells a fully static executable either way: a non-PIE as "statically
        # linked", a modern musl PIE as "static-pie linked". Both have no NEEDED and no
        # PT_INTERP, so both run on any glibc. Anything with an interpreter is dynamic.
        *static*linked*) ;;
        *) fail "bin/$bin is not statically linked, it would die on any host with an older glibc" ;;
    esac
done

echo "==> [3/4] installing inside a fresh container (glibc 2.36) and compiling+running hello world"
WORK="$(mktemp -d)"
printf 'fn main() -> i64 {\n    print("hello from Sond\\n")\n    %s\n}\n' "$EXPECT" > "$WORK/hello.snd"

docker run --rm -i -e EXPECT="$EXPECT" \
    -e HOME="/root" \
    -v "$TARGET_DIR:/release:ro" \
    -v "$WORK:/work:ro" \
    "$IMAGE" bash -s <<'SCRIPT' || fail "container run (glibc 2.36) could not compile+run the hello world"
set -euo pipefail

echo "   (container) installing cc (build-essential: compilers + libc6-dev startfiles)"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq --no-install-recommends build-essential >/dev/null 2>&1
command -v cc >/dev/null

echo "   (container) installing Sond to ~/.local, the layout the tarball ships"
mkdir -p "$HOME/.local"
tar -xzf /release/sond-x86_64-linux.tar.gz -C "$HOME/.local"
[ -x "$HOME/.local/bin/sondc" ] && [ -x "$HOME/.local/bin/sond" ]
PATH="$HOME/.local/bin:$PATH"

echo "   (container) compiling hello.snd"
mkdir -p /tmp/work && cd /tmp/work
cp /work/hello.snd .
"$HOME/.local/bin/sondc" hello.snd -o hello

echo "   (container) running the hello world binary"
set +e
out="$(./hello)"
code=$?
set -e
printf '   exit=%d stdout=%s\n' "$code" "$(printf '%s' "$out" | tr '\n' '|')"
[ "$code" -eq "$EXPECT" ] || { echo "   wrong exit code: expected $EXPECT, got $code"; exit 1; }
[ "$out" = "hello from Sond" ] || { echo "   wrong stdout: expected 'hello from Sond', got '$out'"; exit 1; }
echo "   (container) PORTABLE-RELEASE OK: compiled, printed, exited $EXPECT"
SCRIPT

echo "==> [4/4] portability check result"
echo "PASS: the release runs on Debian 12 (glibc 2.36) via statically linked binaries"