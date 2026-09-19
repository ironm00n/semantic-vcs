#!/usr/bin/env bash
# The acceptance gate: demo lines 1–12 + the forge (demo-lines.sh), then the
# self-hosting line (demo-lines line 14 / self-host.sh: svc on its own crates,
# named checkout from the store, replay). Exit status is the total number of
# failures. Resolves a relative binary path first because the script cds away.
#
#   demo/run.sh [path/to/svc]            # everything
#   SVC_SKIP_SELF_HOST=1 demo/run.sh     # skip line 14
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${1:-}" ]; then
  dir="$(cd "$(dirname "$1")" && pwd)"
  SVC="$dir/$(basename "$1")"
else
  SVC="$HERE/../target/debug/svc"
fi
"$HERE/demo-lines.sh" "$SVC"
fail=$?
echo
echo "gate: $fail failure(s) in total"
exit "$fail"
