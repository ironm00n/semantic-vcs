#!/usr/bin/env bash
# The acceptance gate: demo lines 1–12 + the forge + the self-hosting line 14
# (demo-lines.sh: svc on its own crates, named checkout from the store, replay),
# then the multi-process stress on one store (store-stress.sh), 32 checkouts
# publishing at once (tests/concurrency/workspace_stress.sh) and renames SIGKILLed
# at random points (crash.sh: the store is never a snapshot ahead of the log, a killed
# render is finished by the next open), and two checkouts of this repository itself
# making svc changes to one file at once, merged, one conflict resolved, replayed, and
# type-checked (selfhost-concurrent.sh). Then O9, the compiler oracle over the binder
# table (alpha-rename every local in svc-core, cargo check): it is #[ignore]d in the
# unit suite because it shells out to a second cargo, so this is the only gate that
# runs it. SVC_SKIP_O9=1 skips it.
# Exit status is the total number of failures. Resolves a relative binary path
# first because the scripts cd away.
#
#   demo/run.sh [path/to/svc]            # everything
#   SVC_SKIP_SELF_HOST=1 demo/run.sh     # skip line 14
#   SVC_SKIP_SELFHOST_FULL=1 demo/run.sh # skip M1 whole-repo fidelity
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
SVC_BIN="$SVC" "$HERE/../tests/concurrency/workspace_stress.sh" 32
fail=$((fail + $?))
echo
"$HERE/crash.sh" "$SVC"
fail=$((fail + $?))
echo
"$HERE/selfhost-concurrent.sh" "$SVC"
fail=$((fail + $?))
echo
if [ -z "${SVC_SKIP_O9:-}" ]; then
  if (cd "$HERE/.." && cargo test -q -p svc-core --test o9_compiler_oracle -- --ignored >/dev/null 2>&1); then
    echo "PASS  O9 alpha-renamed svc-core still compiles"
  else
    echo "FAIL  O9 alpha-renamed svc-core still compiles (run: cargo test -p svc-core --test o9_compiler_oracle -- --ignored --nocapture)"
    fail=$((fail + 1))
  fi
  echo
fi
if [ -z "${SVC_SKIP_SELFHOST_FULL:-}" ] && [ -x "$HERE/selfhost-full.sh" ]; then
  if "$HERE/selfhost-full.sh" "$SVC"; then
    echo "PASS  selfhost-full / whole-repo fidelity"
  else
    echo "FAIL  selfhost-full / whole-repo fidelity"
    fail=$((fail + 1))
  fi
  echo
fi
echo "gate: $fail failure(s) in total"
exit "$fail"
