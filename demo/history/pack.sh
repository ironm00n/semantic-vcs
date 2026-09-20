#!/usr/bin/env bash
# Replay every bundle into a fresh store and pack the result — the tree and its .svc —
# as artifacts/history-store.tar.xz with a manifest (the bundles it holds, the op count),
# so demo/dogfood.sh --tui opens this repository's history in seconds and can tell a
# stale pack from a current one. The replay itself is the proof; the pack is a copy of it.
#
#   cargo build -p svc && demo/history/pack.sh [path/to/svc]
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
if [ -n "${1:-}" ]; then SVC="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; else SVC="$ROOT/target/debug/svc"; fi
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
command -v xz >/dev/null || { echo "xz is required"; exit 2; }
WORK="$(mktemp -d "${TMPDIR:-/tmp}/svc-pack.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
"$HERE/replay.sh" "$WORK/history" "$SVC"
ops="$(cd "$WORK/history" && "$SVC" op log --json | jq length)"
# The forge catalog is a derived file the replay wrote; it is large and regenerable.
rm -f "$WORK/history/.svc/forge.json" "$WORK/history/.svc/checkout.lock"
mkdir -p "$ROOT/artifacts"
tar -C "$WORK/history" -cf - . | xz -T0 -6 > "$ROOT/artifacts/history-store.tar.xz"
ls "$HERE"/[0-9]*.json | xargs -n1 basename | jq -R . | jq -s --argjson ops "$ops" --arg svc "$(cd "$ROOT" && (jj --ignore-working-copy log -r @- --no-graph -T "commit_id.short(8)" 2>/dev/null || git rev-parse --short HEAD 2>/dev/null || echo unknown))" \
  '{bundles: ., ops: $ops, packed_at: (now | todate), svc: $svc}' > "$ROOT/artifacts/history-store.json"
echo "packed $(jq -r '.bundles|length' "$ROOT/artifacts/history-store.json") bundles, $ops ops: $(du -h "$ROOT/artifacts/history-store.tar.xz" | cut -f1) at artifacts/history-store.tar.xz"
