#!/usr/bin/env bash
# publish-file.sh SRC DST [MODE] -- put SRC in place at DST without disturbing whoever is using it.
#
# `install -m 755 new ~/.local/bin/sondc` unlinks the destination and writes a fresh file at the
# same name. A process already running the old binary keeps its inode and is fine. The window in
# between is not: for the length of the write the path is missing, and then short, so a compile
# starting there execs a truncated compiler with nothing to say so. Measured at 48 MB: something
# wrong at the path on 24.330 consecutive looks.
#
# Staging beside the destination and renaming closes it: every `exec` of the path gets the whole
# old file or the whole new one. Beside, not in /tmp -- rename is only atomic within one
# filesystem, and $HOME and tmpfs are two.
set -euo pipefail

SRC="${1:?usage: publish-file.sh SRC DST [MODE]}"
DST="${2:?usage: publish-file.sh SRC DST [MODE]}"
MODE="${3:-755}"

[ -f "$SRC" ] || { echo "publish-file: no file at \`$SRC\`" >&2; exit 1; }

mkdir -p "$(dirname "$DST")"
parent="$(cd "$(dirname "$DST")" && pwd)"
name="$(basename "$DST")"

staged="$(mktemp "$parent/.$name.new.XXXXXX")"
trap 'rm -f "$staged"' EXIT
cp "$SRC" "$staged"
chmod "$MODE" "$staged"
mv -f "$staged" "$parent/$name"
trap - EXIT
