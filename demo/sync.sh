#!/usr/bin/env bash
# Review lives in the repository, not in a forge: a changeset travels between two clones
# with its ops and their verdicts. Clone a makes a changeset (a rename, an edit);
# `svc push` carries it to clone b, whose `svc changeset list` then shows the same ops, the
# same verdicts, the same subjects, from the same times (entity ids are each clone's own,
# mapped by path); a second push after one more op
# sends only that op; `svc pull` brings b's own addition back to a; a review verdict is a note
# on the changeset and travels the same way.
#
#   cargo build -p svc && demo/sync.sh [path/to/svc]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${1:-}" ]; then SVC="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; else SVC="$HERE/../target/debug/svc"; fi
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
fail=0
WORK="$(mktemp -d)"
trap '[ "$fail" -eq 0 ] && rm -rf "$WORK"' EXIT
check() { # check <label> '<shell expression>'
  if eval "$2" >/dev/null 2>&1; then echo "PASS  $1"; else echo "FAIL  $1"; fail=$((fail + 1)); fi
}
A="$WORK/a"; B="$WORK/b"
for d in "$A" "$B"; do mkdir -p "$d" && cp -r "$HERE/config/." "$d/" && rm -rf "$d/.git" "$d/.svc"; done
# The review state of a changeset as both clones must show it: ops, verdicts, subjects,
# times — not op indices, which count each clone's own log.
queue() { (cd "$1" && "$SVC" changeset list --json | jq -c "[.[] | select(.name == \"$2\")][0] | {name, intent, ops: [.ops[] | {op: (.op | if type == \"object\" then keys[0] else . end), declared, observed, flagged, subject, at}]}"); }

echo "== sync 1: clone a records a changeset; clone b is the same tree, empty store"
(cd "$A" && "$SVC" init --json >/dev/null && "$SVC" new --json >/dev/null && "$SVC" changeset begin reviewed --json >/dev/null \
  && "$SVC" rename --entity read --new-name read_file --json >/dev/null \
  && "$SVC" edit-def --entity parse --intent refactor --definition "$(cd "$A" && "$SVC" show-def --entity parse --json | jq -r .text | sed 's/Err(Error::Parse)/Err(Error::Parse) \/\/ reviewed/')" --json >/dev/null \
  && "$SVC" changeset end --json >/dev/null)
check "1a a's changeset has a rename and an edit" '[ "$(queue "$A" reviewed | jq ".ops | length")" = 2 ]'
(cd "$B" && "$SVC" init --json >/dev/null)
cs="$(cd "$A" && "$SVC" changeset list --json | jq -r '.[] | select(.name == "reviewed") | .id')"
check "1b b has no changeset yet" '[ "$(cd "$B" && "$SVC" changeset list --json | jq length)" = 0 ]'

echo "== sync 2: svc push carries it to b"
report="$(cd "$A" && "$SVC" push "$cs" "$B" --json)"
check "2a push sent 2 ops" '[ "$(echo "$report" | jq .sent)" = 2 ]'
check "2b b's tree shows the rename" 'grep -q "read_file" "$B/src/main.rs"'
check "2c b lists the same changeset: ops, verdicts, subjects, times" '[ "$(queue "$A" reviewed)" = "$(queue "$B" reviewed)" ]'
check "2d a second push sends nothing" '[ "$(cd "$A" && "$SVC" push "$cs" "$B" --json | jq .sent)" = 0 ]'

echo "== sync 3: one more op on a; only that travels"
(cd "$A" && "$SVC" changeset reopen "$cs" --json >/dev/null; "$SVC" rename --entity validate --new-name check --json >/dev/null; "$SVC" changeset end --json >/dev/null)
report="$(cd "$A" && "$SVC" push "$cs" "$B" --json)"
check "3a push sent 1 op" '[ "$(echo "$report" | jq .sent)" = 1 ]'
check "3b b matches again" '[ "$(queue "$A" reviewed)" = "$(queue "$B" reviewed)" ]'

echo "== sync 4: b adds to the changeset; a pulls it"
(cd "$B" && "$SVC" changeset reopen "$cs" --json >/dev/null; "$SVC" rename --entity normalize --new-name tidy --json >/dev/null; "$SVC" changeset end --json >/dev/null)
report="$(cd "$A" && "$SVC" pull "$cs" "$B" --json)"
check "4a pull took 1 op" '[ "$(echo "$report" | jq .sent)" = 1 ]'
check "4b a's tree shows b's rename" 'grep -q "tidy" "$A/src/main.rs"'
check "4c both clones list one changeset of 4 ops, identically" '[ "$(queue "$A" reviewed | jq ".ops | length")" = 4 ] && [ "$(queue "$A" reviewed)" = "$(queue "$B" reviewed)" ]'

echo "== sync 5: verdicts are notes on the changeset; they travel the same way"
(cd "$A" && "$SVC" review "$cs" --approve --json >/dev/null)
reviews() { (cd "$1" && "$SVC" changeset show "$2" --json | jq -c '[.reviews[] | {op, at}]'); }
check "5a a's approval is one review on the changeset" '[ "$(reviews "$A" "$cs" | jq length)" = 1 ]'
check "5b push sends the verdict alone" '[ "$(cd "$A" && "$SVC" push "$cs" "$B" --json | jq .sent)" = 1 ]'
check "5c b shows the same review" '[ "$(reviews "$A" "$cs")" = "$(reviews "$B" "$cs")" ]'
(cd "$B" && "$SVC" review "$cs" --request-changes --json >/dev/null)
check "5d pull brings b's request for changes back" '[ "$(cd "$A" && "$SVC" pull "$cs" "$B" --json | jq .sent)" = 1 ] && [ "$(reviews "$A" "$cs" | jq length)" = 2 ] && [ "$(reviews "$A" "$cs")" = "$(reviews "$B" "$cs")" ]'

[ "$fail" -eq 0 ] && echo "sync: 0 failures" || { echo "sync: $fail failure(s); scratch kept at $WORK"; }
exit "$fail"
