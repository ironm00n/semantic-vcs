#!/usr/bin/env bash
# Process-level concurrency on one store. WRITERS named checkouts share the store; each
# checkout's `svc` process renames its own entity EACH times on its own change, all at
# once. The store is shared (redb MultiWriter: write transactions serialise on the file,
# a publish that finds a head moved is refused); a checkout is one working copy, held by
# one `svc` at a time, so writers never read each other's half-rendered files. The checks
# are what a reviewer can read off the CLI afterwards: every rename exits 0, the op log
# grows by exactly that many contiguous entries, each checkout shows its own last name
# and nothing of the others, every checkout is clean and not stale; readers with a 1 ms wait
# all get in while the writers keep publishing; and in one checkout the only failure mode
# is `checkout busy`.
#
#   cargo build -p svc && demo/store-stress.sh [path/to/svc] [WRITERS] [EACH]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${1:-}" ]; then SVC="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; else SVC="$HERE/../target/debug/svc"; fi
WRITERS="${2:-6}"
EACH="${3:-5}"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
fail=0
WORK="$(mktemp -d)"
trap '[ "$fail" -eq 0 ] && rm -rf "$WORK" "$WORK"-*' EXIT   # scratch (checkouts, results) is kept only on failure
cp -r "$HERE/config/." "$WORK/" && rm -rf "$WORK/.git" "$WORK/.svc"
cd "$WORK"
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

echo "== the store is shared: 12 readers one after another, 1 ms wait, while $WRITERS checkouts write $EACH describes each"
rm -f "$OUT"/busy-*
for ((w = 0; w < WRITERS; w++)); do
  (cd "$WORK-w$w" && for ((i = 1; i <= EACH; i++)); do SVC_LOCK_TIMEOUT_MS=60000 svcj describe "again $w $i" >/dev/null 2>&1 || echo fail >"$OUT/again-$w-$i.rc"; done) &
done
for ((p = 0; p < 12; p++)); do
  SVC_LOCK_TIMEOUT_MS=1 "$SVC" heads --json >"$OUT/busy-$p.out" 2>"$OUT/busy-$p.err"; echo "$?" >"$OUT/busy-$p.rc"
done
wait
readers_in="$(cat "$OUT"/busy-*.rc | grep -c '^0$')"
log2="$(svcj op log | jq length)"
check "S6 every reader got in ($readers_in of 12)"                        'test "$readers_in" -eq 12'
check "S7 the writers landed $total more ops meanwhile"                   'test "$log2" -eq $((before + 2 * total))'

echo "== one checkout is one session: 8 \`status\` in checkout w0 at once, 1 ms wait"
rm -f "$OUT"/same-*
for ((p = 0; p < 8; p++)); do
  ( cd "$WORK-w0" && SVC_LOCK_TIMEOUT_MS=1 "$SVC" status --json >"$OUT/same-$p.out" 2>"$OUT/same-$p.err"; echo "$?" >"$OUT/same-$p.rc" ) &
done
wait
succeeded="$(cat "$OUT"/same-*.rc | grep -c '^0$')"
failed="$(cat "$OUT"/same-*.rc | grep -vc '^0$')"
busy="$(grep -l "checkout busy" "$OUT"/same-*.err 2>/dev/null | wc -l)"
check "S8 at least one got in ($succeeded of 8)"                      'test "$succeeded" -ge 1'
check "S9 every refusal says checkout busy ($busy of $failed)"        'test "$busy" -eq "$failed"'

echo
if [ "$fail" -eq 0 ]; then echo "0 failures"; else echo "$fail failure(s); stress scratch kept at $WORK"; fi
exit $fail
