#!/usr/bin/env bash
# Print the directory where `cargo build --release` of the compiler leaves its binaries.
# Asked of cargo, not assumed: `build.target-dir` in a cargo config or CARGO_TARGET_DIR move it
# away from compiler/target/, and a script that assumed the default published stale binaries.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
if ! command -v cargo >/dev/null 2>&1 && [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]; then
  PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
fi
target="$(cargo metadata --format-version 1 --no-deps --manifest-path "$REPO/compiler/Cargo.toml" \
  | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
printf '%s/release\n' "$target"
