#!/usr/bin/env bash
# The acceptance gate: SPEC §10 lines 1–12 + the forge (demo-lines.sh), then the
# self-hosting line (self-host.sh: svc on its own crates, three real cargo builds,
# ~80 s). Exit status is the total number of failures. Resolves a relative binary
# path first because both scripts cd away.
#
#   demo/run.sh [path/to/svc]            # everything
#   SVC_SKIP_SELF_HOST=1 demo/run.sh     # the fast part only
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${1:-}" ]; then
  dir="$(cd "$(dirname "$1")" && pwd)"
  SVC="$dir/$(basename "$1")"
else
  SVC="$HERE/../target/debug/svc"
fi
"$HERE/demo-lines.sh" "$SVC"
fail=$?
if [ -z "${SVC_SKIP_SELF_HOST:-}" ]; then
  echo
  "$HERE/self-host.sh" "$SVC"
  fail=$((fail + $?))
fi
echo
echo "gate: $fail failure(s) in total"
exit "$fail"
