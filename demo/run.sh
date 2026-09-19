#!/usr/bin/env bash
# The acceptance gate: demo lines 1–12 + the forge + the self-hosting line 14
# (demo-lines.sh: svc on its own crates, named checkout from the store, replay),
# then the multi-process stress on one store (store-stress.sh). Exit status is
# the total number of failures. Resolves a relative binary path first because
# the scripts cd away.
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
"$HERE/store-stress.sh" "$SVC"
fail=$((fail + $?))
echo
echo "gate: $fail failure(s) in total"
exit "$fail"
