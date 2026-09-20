#!/usr/bin/env bash
# Re-cut every bundle from a full replay, with the current exporter: each op then carries
# the files it changed, so a later engine that computes an op differently no longer breaks
# the replay (the record supplies the tree). Bundles keep their names, times, changesets and
# checkouts. Stops at the first bundle that does not reproduce; the ones before it are
# rewritten in place.
#
#   demo/history/refresh.sh [path/to/svc]      # rewrites demo/history/*.json
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
if [ -n "${1:-}" ]; then SVC="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; else SVC="$ROOT/target/debug/svc"; fi
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
GITDIR="$(cd "$ROOT" && (jj git root 2>/dev/null || echo "$ROOT/.git"))"
TOOL="$ROOT/target/debug/examples/history"
[ -x "$TOOL" ] || (cd "$ROOT" && cargo build -q -p svc-repo --example history) || exit 2
DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT
cd "$DIR" || exit 2
ordered="$(for bundle in "$HERE"/[0-9][0-9][0-9][0-9]-*.json; do
  [ -e "$bundle" ] || continue
  base="$(basename "$bundle" .json | cut -d- -f2)"
  depth="$(git --git-dir="$GITDIR" rev-list --count "$base" 2>/dev/null || echo 0)"
  printf "%08d %s\n" "$depth" "$bundle"
done | sort | cut -d" " -f2-)"
bad=0
for bundle in $ordered; do
  name="$(basename "$bundle" .json)"
  base="$(echo "$name" | cut -d- -f2)"
  find "$DIR" -mindepth 1 -maxdepth 1 ! -name .svc -exec rm -rf {} +
  git --git-dir="$GITDIR" archive "$base" | tar -x -C "$DIR" || { echo "no git tree for $base"; exit 2; }
  if [ ! -d .svc ]; then "$SVC" init --json >/dev/null || exit 2; else "$SVC" status --json >/dev/null || exit 2; fi
  start="$("$SVC" op log --json | jq length)"
  if ! report="$("$SVC" history import "$bundle" --json)"; then echo "FAIL  $name: $report"; bad=$((bad + 1)); continue; fi
  if [ "$(echo "$report" | jq -r '.diverged_at')" != null ]; then echo "FAIL  $name: diverged; not rewritten"; bad=$((bad + 1)); continue; fi
  end="$("$SVC" op log --json | jq length)"
  "$TOOL" export "$start" "$bundle" "$end" >/dev/null || exit 1
  echo "PASS  $name: ops $start..$end re-cut with files for every op ($(wc -c <"$bundle") bytes)"
done
[ "$bad" -eq 0 ] || { echo "$bad bundle(s) not rewritten"; exit 1; }
