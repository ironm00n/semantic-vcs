#!/usr/bin/env bash
# Process-level concurrency on one store. WRITERS named checkouts share the store; each
# checkout's `svc` process renames its own entity EACH times on its own change, all at
# once. The store's exclusive session serialises them with a bounded wait; a checkout is
# one working copy, so writers never read each other's half-rendered files. The checks
# are what a reviewer can read off the CLI afterwards: every rename exits 0, the op log
# grows by exactly that many contiguous entries, each checkout shows its own last name
# and nothing of the others, every checkout is clean and not stale, and with a 1 ms lock
# wait the only failure mode is `store busy`.
#
#   cargo build -p svc && demo/store-stress.sh [path/to/svc] [WRITERS] [EACH]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
SVC="${1:-$HERE/../target/debug/svc}"
WRITERS="${2:-6}"
EACH="${3:-5}"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
WORK="$(mktemp -d)"
cp -r "$HERE/config/." "$WORK/" && rm -rf "$WORK/.git" "$WORK/.svc"
cd "$WORK"
fail=0
check() { # check <label> '<shell expression>'
  if eval "$2" >/dev/null 2>&1; then echo "PASS  $1"; else echo "FAIL  $1"; fail=$((fail + 1)); fi
}
svcj() { "$SVC" "$@" --json; }
# One entity per writer, so a writer's name resolution depends only on its own history.
entities=(read parse validate normalize log canon load main)
[ "$WRITERS" -le "${#entities[@]}" ] || { echo "at most ${#entities[@]} writers"; exit 2; }

svcj init >/dev/null
svcj new >/dev/null
OUT="$WORK-out"; mkdir -p "$OUT"   # results and checkouts live beside the working copy, not inside it
for ((w = 0; w < WRITERS; w++)); do
  svcj workspace add "w$w" "$WORK-w$w" >/dev/null
  (cd "$WORK-w$w" && svcj new >/dev/null)   # its own change: no checkout is ever stale
done
before="$(svcj op log | jq length)"

echo "== $WRITERS checkouts × $EACH renames at once, one store, 60 s lock wait"
for ((w = 0; w < WRITERS; w++)); do
  (
    cd "$WORK-w$w"
    name="${entities[$w]}"
    for ((i = 1; i <= EACH; i++)); do
      next="${entities[$w]}_$i"
      if SVC_LOCK_TIMEOUT_MS=60000 "$SVC" rename --entity "$name" --new-name "$next" --json >"$OUT/w$w-$i.json" 2>"$OUT/w$w-$i.err"; then
        echo ok >"$OUT/w$w-$i.rc"
      else
        echo "$?" >"$OUT/w$w-$i.rc"
      fi
      name="$next"
    done
  ) &
done
wait

total=$((WRITERS * EACH))
ok="$(cat "$OUT"/*.rc | grep -c '^ok$')"
log="$(svcj op log)"
check "S1 every rename exited 0 ($ok/$total)" 'test "$ok" -eq "$total"'
check "S2 op log grew by exactly $total, indices contiguous" 'echo "$log" | jq -e --argjson b "$before" --argjson n "$total" "length == \$b + \$n and ([.[].ix] | sort) == [range(length)]"'
own_names_ok() { # each checkout: its own last name, none of its intermediates, none of the others' renames
  for ((w = 0; w < WRITERS; w++)); do
    f="$WORK-w$w/src/main.rs"
    grep -q "fn ${entities[$w]}_$EACH(" "$f" || return 1
    for ((i = 1; i < EACH; i++)); do grep -q "fn ${entities[$w]}_$i(" "$f" && return 1; done
    for ((o = 0; o < WRITERS; o++)); do [ "$o" = "$w" ] || ! grep -q "fn ${entities[$o]}_" "$f" || return 1; done
  done
  return 0
}
check "S3 each checkout shows only its own final rename"  'own_names_ok'
check "S4 every checkout clean and not stale"             'for ((w = 0; w < WRITERS; w++)); do (cd "$WORK-w$w" && svcj status | jq -e ".semantic == 0 and .layout == 0") || exit 1; done; svcj workspace list | jq -e "all(.stale == false)"'
check "S5 the default checkout saw none of it"            'svcj status | jq -e ".semantic == 0 and .layout == 0" && ! grep -q "_$EACH(" src/main.rs'

echo "== 1 ms lock wait: the only failure mode is \`store busy\`"
rm -f "$OUT"/busy-*
for ((p = 0; p < 12; p++)); do
  ( SVC_LOCK_TIMEOUT_MS=1 "$SVC" heads --json >"$OUT/busy-$p.out" 2>"$OUT/busy-$p.err"; echo "$?" >"$OUT/busy-$p.rc" ) &
done
wait
succeeded="$(cat "$OUT"/busy-*.rc | grep -c '^0$')"
failed="$(cat "$OUT"/busy-*.rc | grep -vc '^0$')"
busy="$(grep -l "store busy" "$OUT"/busy-*.err 2>/dev/null | wc -l)"
check "S6 at least one reader got in ($succeeded of 12)"       'test "$succeeded" -ge 1'
check "S7 every refusal says store busy ($busy of $failed)"    'test "$busy" -eq "$failed"'

echo
echo "$fail failure(s); stress scratch at $WORK"
exit $fail
