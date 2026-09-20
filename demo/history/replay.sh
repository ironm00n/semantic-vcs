#!/usr/bin/env bash
# Rebuild this repository's svc history into one store: for each bundle in demo/history,
# in order, materialise git's tree at the bundle's base commit into the target directory
# (other people's landings in between become one absorbed hand edit), then replay the
# bundle. Exits non-zero at the first bundle that does not reproduce its recorded trees.
#
#   demo/history/replay.sh <dir> [path/to/svc]      # <dir> is created; .svc lands inside
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DIR="${1:?target directory}"
if [ -n "${2:-}" ]; then SVC="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"; else SVC="$ROOT/target/debug/svc"; fi
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
# A jj workspace has no .git of its own; the colocated repository does.
GITDIR="$(cd "$ROOT" && (jj git root 2>/dev/null || echo "$ROOT/.git"))"
# The CLI arm when it exists, the library tool otherwise.
if "$SVC" history --help >/dev/null 2>&1; then
  import() { "$SVC" history import "$1" --json; }
else
  TOOL="$ROOT/target/debug/examples/history"
  [ -x "$TOOL" ] || (cd "$ROOT" && cargo build -q -p svc-repo --example history) || exit 2
  import() { "$TOOL" import "$1"; }
fi
mkdir -p "$DIR"
DIR="$(cd "$DIR" && pwd)"
cd "$DIR"
n=0
for bundle in "$HERE"/[0-9][0-9][0-9][0-9]-*.json; do
  [ -e "$bundle" ] || { echo "no bundles in $HERE"; exit 2; }
  name="$(basename "$bundle" .json)"
  base="$(echo "$name" | cut -d- -f2)"
  # git's tree at the base: replace every tracked file, drop the ones no longer there.
  find . -mindepth 1 -maxdepth 1 ! -name .svc -exec rm -rf {} +
  git --git-dir="$GITDIR" archive "$base" | tar -x -C "$DIR" || { echo "no git tree for $base"; exit 2; }
  if [ ! -d .svc ]; then
    "$SVC" init --json >/dev/null || exit 2
  else
    "$SVC" status --json >/dev/null || exit 2    # everyone else's landings, absorbed
  fi
  report="$(import "$bundle")" || { echo "FAIL  $name: $report"; exit 1; }
  diverged="$(echo "$report" | jq -r '.diverged_at')"
  applied="$(echo "$report" | jq -r '.applied')"
  if [ "$diverged" != null ]; then echo "FAIL  $name: diverged at op $diverged"; exit 1; fi
  echo "PASS  $name: $applied ops replayed, every tree as recorded"
  n=$((n + 1))
done
echo "$n bundle(s); $("$SVC" op log --json | jq length) ops in $DIR/.svc"
