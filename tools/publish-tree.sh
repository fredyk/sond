#!/usr/bin/env bash
# publish-tree.sh SRC DST -- replace the published tree at DST with a copy of SRC.
#
# The point is the ORDER. Deleting DST and then copying into it publishes nothing for as long
# as the copy takes, and a copy that stops halfway leaves a fragment published with nothing to
# say so. Both were live: ~/.local/lib/sond is read by every compile on this machine.
#
# So DST is a symlink to a timestamped tree, and publishing is: build the new tree beside it,
# point a temporary link at it, and rename that link onto DST. A rename over an existing name
# is atomic, so a reader gets the whole old tree or the whole new one, never a gap and never a
# fragment. The previous tree is removed only after the swap has happened.
#
# The one exception is the first publish over a DST that is still a real directory: there the
# old directory has to be moved aside before the link can take its name. That gap is two
# renames wide, not one copy wide, and it happens once.
#
# --as-directory leaves DST a real directory instead of a symlink, swapped in by the same two
# renames. It is for a destination that something else insists on being a directory -- a path
# git tracks, say -- and it buys all-or-nothing without buying gap-free. Say which one the call
# site needs; do not guess from the path.
set -euo pipefail

COMO_DIRECTORIO=0
if [ "${1:-}" = "--as-directory" ]; then COMO_DIRECTORIO=1; shift; fi

SRC="${1:?usage: publish-tree.sh [--as-directory] SRC DST}"
DST="${2:?usage: publish-tree.sh [--as-directory] SRC DST}"

[ -d "$SRC" ] || { echo "publish-tree: no source tree at \`$SRC\`" >&2; exit 1; }

parent="$(cd "$(dirname "$DST")" 2>/dev/null && pwd)" || {
  mkdir -p "$(dirname "$DST")"
  parent="$(cd "$(dirname "$DST")" && pwd)"
}
name="$(basename "$DST")"
DST="$parent/$name"

# `.arbol.` and not `.nuevo.`: in the symlink case this directory does not go away when
# the publish ends -- it IS the published tree from then on, and the link points at it
# until the next publish. A name that says "new" would read as leftover staging to the
# next person who looks, and deleting it takes the library down.
nuevo="$(mktemp -d "$parent/.$name.arbol.XXXXXX")"
enlace="$parent/.$name.enlace.$$"
retirado=""
limpiar() { rm -rf ${nuevo:+"$nuevo"} "$enlace" ${retirado:+"$retirado"}; }
trap limpiar EXIT

cp -R "$SRC/." "$nuevo/"
chmod 755 "$nuevo"

# What DST points at right now, so it can be removed AFTER the swap and not before.
anterior=""
if [ -L "$DST" ]; then
  anterior="$(cd "$parent" && readlink "$name")"
  case "$anterior" in /*) : ;; *) anterior="$parent/$anterior" ;; esac
fi

if [ "$COMO_DIRECTORIO" = 1 ]; then
  if [ -e "$DST" ] || [ -L "$DST" ]; then
    retirado="$(mktemp -d "$parent/.$name.retirado.XXXXXX")"
    rmdir "$retirado"
    mv "$DST" "$retirado"
  fi
  mv -T "$nuevo" "$DST"
  nuevo=""
  trap - EXIT
  [ -n "$anterior" ] && rm -rf "$anterior"
  [ -n "$retirado" ] && rm -rf "$retirado"
  exit 0
fi

ln -s "$nuevo" "$enlace"
if [ -d "$DST" ] && [ ! -L "$DST" ]; then
  # First publish over a real directory: it has to give up the name before the link can take it.
  retirado="$(mktemp -d "$parent/.$name.retirado.XXXXXX")"
  rmdir "$retirado"
  mv "$DST" "$retirado"
fi
mv -T "$enlace" "$DST"

trap - EXIT
[ -n "$anterior" ] && [ "$anterior" != "$nuevo" ] && rm -rf "$anterior"
[ -n "$retirado" ] && rm -rf "$retirado"
exit 0
