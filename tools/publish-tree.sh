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

AS_DIRECTORY=0
if [ "${1:-}" = "--as-directory" ]; then AS_DIRECTORY=1; shift; fi

SRC="${1:?usage: publish-tree.sh [--as-directory] SRC DST}"
DST="${2:?usage: publish-tree.sh [--as-directory] SRC DST}"

[ -d "$SRC" ] || { echo "publish-tree: no source tree at \`$SRC\`" >&2; exit 1; }

parent="$(cd "$(dirname "$DST")" 2>/dev/null && pwd)" || {
  mkdir -p "$(dirname "$DST")"
  parent="$(cd "$(dirname "$DST")" && pwd)"
}
name="$(basename "$DST")"
DST="$parent/$name"

# `.tree.` and not `.new.`: in the symlink case this directory does not go away when
# the publish ends -- it IS the published tree from then on, and the link points at it
# until the next publish. A name that says "new" would read as leftover staging to the
# next person who looks, and deleting it takes the library down.
staged="$(mktemp -d "$parent/.$name.tree.XXXXXX")"
link="$parent/.$name.link.$$"
stepped_aside=""
cleanup() { rm -rf ${staged:+"$staged"} "$link" ${stepped_aside:+"$stepped_aside"}; }
trap cleanup EXIT

cp -R "$SRC/." "$staged/"
chmod 755 "$staged"

# What DST points at right now, so it can be removed AFTER the swap and not before.
previous=""
if [ -L "$DST" ]; then
  previous="$(cd "$parent" && readlink "$name")"
  case "$previous" in /*) : ;; *) previous="$parent/$previous" ;; esac
fi

if [ "$AS_DIRECTORY" = 1 ]; then
  if [ -e "$DST" ] || [ -L "$DST" ]; then
    stepped_aside="$(mktemp -d "$parent/.$name.retirado.XXXXXX")"
    rmdir "$stepped_aside"
    mv "$DST" "$stepped_aside"
  fi
  mv -T "$staged" "$DST"
  staged=""
  trap - EXIT
  [ -n "$previous" ] && rm -rf "$previous"
  [ -n "$stepped_aside" ] && rm -rf "$stepped_aside"
  exit 0
fi

ln -s "$staged" "$link"
if [ -d "$DST" ] && [ ! -L "$DST" ]; then
  # First publish over a real directory: it has to give up the name before the link can take it.
  stepped_aside="$(mktemp -d "$parent/.$name.retirado.XXXXXX")"
  rmdir "$stepped_aside"
  mv "$DST" "$stepped_aside"
fi
mv -T "$link" "$DST"

trap - EXIT
[ -n "$previous" ] && [ "$previous" != "$staged" ] && rm -rf "$previous"
[ -n "$stepped_aside" ] && rm -rf "$stepped_aside"
exit 0
