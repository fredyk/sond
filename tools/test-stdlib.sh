#!/usr/bin/env bash
# Run `sond test` on every library that carries `test "..."` blocks, enforcing the mandatory
# coverage gate. This is the standard library dogfooding its own testing model.
#
# Uses the `sond` driver from $SOND (default: the repo's release build, else PATH).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SOND="${SOND:-$("$REPO/tools/release-dir.sh")/sond}"
[ -x "$SOND" ] || SOND=sond

fail=0
while IFS= read -r f; do
    printf '== %s\n' "${f#"$REPO"/}"
    "$SOND" test "$f" || fail=1
    echo
    # `git ls-files` rather than a recursive grep: on at least one developer machine `grep` is a
    # wrapper that skips directories without saying so, and a test suite that silently stops
    # looking is worse than no test suite. The index knows exactly what is tracked.
done < <(cd "$REPO" && git ls-files 'lib/**/*_test.snd' 'lib/*_test.snd' \
         | while IFS= read -r f; do grep -q '^test "' "$REPO/$f" && printf '%s/%s\n' "$REPO" "$f"; done \
         | sort)

if [ "$fail" -eq 0 ]; then
    echo "all standard-library tests passed with >= their coverage threshold"
else
    echo "some standard-library tests failed"
    exit 1
fi
