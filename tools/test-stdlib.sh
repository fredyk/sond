#!/usr/bin/env bash
# Run `sond test` on every library that carries `test "..."` blocks, enforcing the mandatory
# coverage gate. This is the standard library dogfooding its own testing model.
#
# Uses the `sond` driver from $SOND (default: the repo's release build, else PATH).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SOND="${SOND:-$REPO/compiler/target/release/sond}"
[ -x "$SOND" ] || SOND=sond

fail=0
while IFS= read -r f; do
    printf '== %s\n' "${f#"$REPO"/}"
    "$SOND" test "$f" || fail=1
    echo
done < <(grep -rl '^test "' "$REPO/lib" --include='*_test.snd' | sort)

if [ "$fail" -eq 0 ]; then
    echo "all standard-library tests passed with >= their coverage threshold"
else
    echo "some standard-library tests failed"
    exit 1
fi
