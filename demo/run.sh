#!/usr/bin/env bash
# SPEC §10 acceptance. Resolves a relative binary path before demo-lines.sh cds away.
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${1:-}" ]; then
  dir="$(cd "$(dirname "$1")" && pwd)"
  exec "$HERE/demo-lines.sh" "$dir/$(basename "$1")"
fi
exec "$HERE/demo-lines.sh"
