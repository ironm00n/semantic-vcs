#!/usr/bin/env bash
# Rebuild this repository's svc history into one store: for each bundle in demo/history,
# in order, materialise git's tree at the bundle's base commit into the target directory
# (other people's landings in between become one absorbed hand edit), then replay the
# bundle. Exits non-zero at the first bundle that does not reproduce its recorded trees.
#
#   demo/history/replay.sh <dir> [path/to/svc]      # <dir> is created; .svc lands inside
set -euo pipefail
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
if [ -e "$DIR" ] && [ ! -d "$DIR" ]; then
  echo "refusing replay target that is not a directory: $DIR" >&2
  exit 2
fi
mkdir -p -- "$DIR"
cd -P -- "$DIR"
DIR="$(pwd -P)"
first_entry="$(find . -mindepth 1 -maxdepth 1 -print -quit)"
if [ -n "$first_entry" ]; then
  echo "refusing nonempty replay target: $DIR (choose a new or empty directory)" >&2
  exit 2
fi
# Landing order is the base commit's depth in history, not the file name: two agents
# can pick the same number, and a later base always sits deeper on main.
ordered="$(for bundle in "$HERE"/[0-9][0-9][0-9][0-9]-*.json; do
  [ -e "$bundle" ] || continue
  base="$(basename "$bundle" .json | cut -d- -f2)"
  depth="$(git --git-dir="$GITDIR" rev-list --count "$base" 2>/dev/null || echo 0)"
  printf "%08d %s\n" "$depth" "$bundle"
done | sort | cut -d" " -f2-)"
[ -n "$ordered" ] || { echo "no bundles in $HERE"; exit 2; }
n=0
for bundle in $ordered; do
  name="$(basename "$bundle" .json)"
  base="$(echo "$name" | cut -d- -f2)"
  git --git-dir="$GITDIR" cat-file -e "$base^{tree}" || { echo "no git tree for $base"; exit 2; }
  # git's tree at the base: replace every tracked file, drop the ones no longer there.
  find . -mindepth 1 -maxdepth 1 ! -name .svc -exec rm -rf {} +
  git --git-dir="$GITDIR" archive "$base" | tar -xm -C "$DIR" || { echo "no git tree for $base"; exit 2; }
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
"$SVC" forge export --json >/dev/null 2>&1   # the catalog the forge serves, beside the store
summary="$("$SVC" op log --json | jq -r '[.[] | (.op | if type == "object" then keys[0] else . end)] | group_by(.) | map("\(length) \(.[0])") | join(", ")')"
echo "$n bundle(s); $("$SVC" op log --json | jq length) ops in $DIR/.svc — $summary (forge catalog: .svc/forge.json)"
