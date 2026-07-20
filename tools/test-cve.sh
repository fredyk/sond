#!/usr/bin/env bash
# Compile a corpus of real published vulnerabilities, ported faithfully to Sond.
#
# Each case is a pair. `vuln.snd` carries the bug as it shipped, written the way the original
# was written and with no contracts the original did not have. `fixed.snd` carries the change
# upstream actually made. The claim under test is narrow and falsifiable:
#
#   the vulnerable version must be REJECTED, and the fixed version must COMPILE
#
# The second half is what makes the first half mean anything. A compiler that rejects everything
# proves nothing, so a case whose fix does not build is reported as INVALID rather than as a win.
#
# Cases the compiler does not catch are listed as MISSED and do not fail this script. They are
# the honest part of the corpus: the point is to know exactly where the guarantee stops, and to
# be told the day it moves in either direction.
#
#   tools/test-cve.sh
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SONDC="${SONDC:-$HOME/.local/bin/sondc}"
[ -x "$SONDC" ] || SONDC="$REPO/compiler/target/release/sondc"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

caught=0 missed=0 invalid=0 total=0

printf '%-20s %-11s %-11s %s\n' "CASE" "VULNERABLE" "FIXED" "VERDICT"
printf '%s\n' "----------------------------------------------------------------------"

for dir in "$REPO"/tests/cve/*/; do
    name="$(basename "$dir")"
    [ -f "$dir/vuln.snd" ] && [ -f "$dir/fixed.snd" ] || continue
    total=$((total + 1))

    # Full compilation: --emit-header stops after parsing and typing, and would never run the
    # contract prover that most of these cases turn on.
    timeout 120 "$SONDC" "$dir/vuln.snd"  --shared -o "$WORK/v.so" >"$WORK/v.err" 2>&1; v=$?
    timeout 120 "$SONDC" "$dir/fixed.snd" --shared -o "$WORK/f.so" >"$WORK/f.err" 2>&1; f=$?

    if   [ $v -ne 0 ] && [ $f -eq 0 ]; then verdict="caught";  caught=$((caught + 1))
    elif [ $v -eq 0 ] && [ $f -eq 0 ]; then verdict="MISSED";  missed=$((missed + 1))
    else                                    verdict="INVALID"; invalid=$((invalid + 1))
    fi

    printf '%-20s %-11s %-11s %s\n' \
        "$name" \
        "$([ $v -ne 0 ] && echo rejected || echo compiles)" \
        "$([ $f -eq 0 ] && echo compiles || echo REJECTED)" \
        "$verdict"

    [ "$verdict" = "caught"  ] && sed -n '1p' "$WORK/v.err" | cut -c1-100 | sed 's/^/    /'
    [ "$verdict" = "INVALID" ] && sed -n '1p' "$WORK/f.err" | cut -c1-100 | sed 's/^/    the fix does not build: /'
done

echo
echo "caught $caught of $total, missed $missed, invalid $invalid"

# A case whose fix stopped compiling means the corpus itself is broken, and that is a failure.
# A missed vulnerability is a known limit, recorded in tests/cve/README.md, not a build break.
if [ "$invalid" -gt 0 ]; then
    echo "FAILED: a fixed version no longer builds, so its case proves nothing"
    exit 1
fi
