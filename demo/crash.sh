#!/usr/bin/env bash
# Crash recovery at process level. A tree big enough that a rename takes a few hundred
# milliseconds; TRIALS times, `svc rename` is SIGKILLed at a random point of its run. After
# every kill the store must be usable and honest: `svc status` exits 0 and is clean (a
# killed render is finished by the next open), the op log grew by 0 or 1 — never a
# snapshot ahead of the log — and the tree shows the new name exactly when the op landed.
#
#   cargo build -p svc && demo/crash.sh [path/to/svc] [TRIALS] [FILES]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${1:-}" ]; then SVC="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; else SVC="$HERE/../target/debug/svc"; fi
TRIALS="${2:-12}"
FILES="${3:-150}"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
fail=0
WORK="$(mktemp -d)"
trap '[ "$fail" -eq 0 ] && rm -rf "$WORK"' EXIT
cp -r "$HERE/config/." "$WORK/" && rm -rf "$WORK/.git" "$WORK/.svc"
cd "$WORK"
check() { # check <label> '<shell expression>'
  if eval "$2" >/dev/null 2>&1; then echo "PASS  $1"; else echo "FAIL  $1"; fail=$((fail + 1)); fi
}
svcj() { "$SVC" "$@" --json; }
# FILES copies of the demo module with every item suffixed, so each file has its own names.
for ((i = 1; i <= FILES; i++)); do
  sed -E "s/\b(Config|Error|read|parse|validate|normalize|log|canon|load|main)\b/\1_$i/g" src/main.rs >"src/m$i.rs"
done
svcj init >/dev/null
svcj new >/dev/null
echo "== $FILES files, $(svcj status | jq .entities) entities"

# How long an undisturbed rename takes here, so the kills land inside it.
start=$(date +%s%N); svcj rename --entity read_1 --new-name read_1_warm >/dev/null; full_ns=$(( $(date +%s%N) - start ))
full_ms=$((full_ns / 1000000))
echo "== a rename takes ${full_ms} ms; $TRIALS trials, SIGKILL at a random point of it"
before="$(svcj op log | jq length)"
name="read_1_warm"
landed=0; cut_short=0; mid_render=0
for ((t = 1; t <= TRIALS; t++)); do
  next="read_1_t$t"
  # The publish sits in the first part of a rename (absorb, compute, one transaction);
  # the render is the rest. Kills spread over the first two thirds land on both sides.
  at_ms=$(( (RANDOM % (full_ms > 30 ? full_ms * 2 / 3 : 20)) + 10 ))
  rc=$( (timeout -s KILL "0.$(printf '%03d' "$at_ms")" "$SVC" rename --entity "$name" --new-name "$next" --json >/dev/null 2>&1; echo $?) 2>/dev/null )
  ok=1
  status="$(svcj status 2>/dev/null)" || ok=0
  [ "$(echo "$status" | jq '.semantic + .layout')" = 0 ] || ok=0
  log="$(svcj op log | jq length)"
  delta=$((log - before))
  if [ "$delta" -eq 1 ]; then
    grep -q "fn $next(" src/m1.rs || ok=0; landed=$((landed + 1)); name="$next"; before="$log"
    [ "$rc" = 137 ] && mid_render=$((mid_render + 1))   # published, then killed: the next open finished the render
  elif [ "$delta" -eq 0 ]; then
    grep -q "fn $name(" src/m1.rs || ok=0; ! grep -q "fn $next(" src/m1.rs || ok=0; cut_short=$((cut_short + 1))
  else
    ok=0
  fi
  [ "$ok" = 1 ] || { echo "     trial $t: killed at ${at_ms} ms (rc $rc), op log +$delta, status: $status"; fail=$((fail + 1)); }
done
check "C1 every trial: status exits 0 and is clean, op log +0 or +1, tree matches ($landed landed — $mid_render of them killed mid-render — $cut_short cut short, of $TRIALS)" 'test "$fail" -eq 0'
check "C2 the kills hit both sides of the publish (needs both; rerun if the machine was too fast or too slow)" 'test "$landed" -ge 1 && test "$cut_short" -ge 1'
check "C3 the store is whole afterwards: replay is clean" 'svcj replay | jq -e ".diverged_at == null"'
echo
if [ "$fail" -eq 0 ]; then echo "0 failures"; else echo "$fail failure(s); crash scratch kept at $WORK"; fi
exit $fail
