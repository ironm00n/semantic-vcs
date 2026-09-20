#!/usr/bin/env bash
# Concurrency on the real thing: this repository's whole tree, one store, two named
# checkouts. At the same time, checkout a renames `RedbStore::open_or_create` and edits
# `RedbStore::open`; checkout b renames `file_hashes` (repo.rs, three call sites) and edits
# the same `open` differently — all through svc verbs. Then a merges b: exactly one content conflict, on
# `open`; `svc resolve` takes b's side; the merged file carries both renames and b's body;
# `svc replay` reproduces the whole log; and (unless SVC_SKIP_SELF_HOST is set) the merged
# crate still type-checks.
#
#   cargo build -p svc && demo/selfhost-concurrent.sh [path/to/svc]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
if [ -n "${1:-}" ]; then SVC="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; else SVC="$ROOT/target/debug/svc"; fi
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
fail=0
WORK="$(mktemp -d)"
trap '[ "$fail" -eq 0 ] && rm -rf "$WORK" "$WORK"-*' EXIT
check() { # check <label> '<shell expression>'
  if eval "$2" >/dev/null 2>&1; then echo "PASS  $1"; else echo "FAIL  $1"; fail=$((fail + 1)); fi
}
svcj() { "$SVC" "$@" --json; }
# The entity with this name in this file, by id: `Type::name` works for verbs that go
# through svc-repo's resolver; the id works everywhere.
id_of() { svcj list-defs | jq -r --arg n "$1" --arg f "$2" '.definitions[] | select(.name == $n and .file == $f) | .id'; }

# The real tree: every tracked file, minus the VCS metadata, build output and the store.
(cd "$ROOT" && tar --exclude=./target --exclude=./.git --exclude=./.jj --exclude=./.svc \
  --exclude='./demo/git-twin/work' -cf - .) | (cd "$WORK" && tar -xf -)
cd "$WORK"
svcj init >/dev/null
entities="$(svcj status | jq .entities)"
echo "== this repository, $entities entities; checkouts a and b on one store"
svcj workspace add a "$WORK-a" >/dev/null
svcj workspace add b "$WORK-b" >/dev/null
STORE="crates/svc-repo/src/store.rs"
REPO="crates/svc-repo/src/repo.rs"
OPEN="$(id_of open "$STORE")"
before="$(svcj op log | jq length)"

# A doc-comment edit is enough to make the two bodies differ; both are made from what
# svc show-def hands back, so each side's text is svc's own rendering plus one line.
edit_open() { # edit_open <marker line inserted before the let>
  svcj show-def --entity "$OPEN" | jq -r .text | sed "s|^        let db = |        $1\n        let db = |"
}
echo "== at once: a renames open_or_create + edits open; b renames file_hashes + edits open"
(
  cd "$WORK-a" && svcj new >/dev/null \
    && svcj rename --entity RedbStore::open_or_create --new-name open_database >/dev/null \
    && svcj edit-def --entity "$OPEN" --definition "$(edit_open '// An existing file: nothing is created.')" --intent refactor >/dev/null
  echo $? >"$WORK-a.rc"
) &
(
  cd "$WORK-b" && svcj new >/dev/null \
    && svcj rename --entity file_hashes --new-name hashes_by_path >/dev/null \
    && svcj edit-def --entity "$OPEN" --definition "$(edit_open '// Never creates; init makes the file.')" --intent refactor >/dev/null
  echo $? >"$WORK-b.rc"
) &
wait
log="$(svcj op log)"
check "C1 both checkouts' verbs exited 0"                       'test "$(cat "$WORK-a.rc")" = 0 && test "$(cat "$WORK-b.rc")" = 0'
check "C2 op log +6 (2 new, 2 renames, 2 edit-defs), contiguous" 'echo "$log" | jq -e --argjson b "$before" "length == \$b + 6 and ([.[].ix] | sort) == [range(length)]"'
check "C3 neither checkout is stale"                             'svcj workspace list | jq -e "all(.stale == false)"'

echo "== a merges b"
B="$(svcj workspace list | jq -r '.[] | select(.name == "b") | .change')"
cd "$WORK-a"
merge="$(svcj merge "$B")"
check "C4 exactly one conflict, a content conflict on open"      'echo "$merge" | jq -e ".conflicts | length == 1" && svcj conflicts | jq -e ".[0].conflict | has(\"Content\")" && svcj conflicts | jq -e ".[0].name == \"open\""'
check "C5 svc status says so"                                    '"$SVC" status | grep -q "1 conflict — svc conflicts"'
svcj resolve 0 --take b >/dev/null
check "C6 resolved: status clean, no conflicts"                  'svcj status | jq -e ".conflicts == 0 and .semantic == 0 and .layout == 0"'
check "C7 the merged file has both renames at every use and b's body" \
  'grep -q "fn open_database(" "$STORE" && ! grep -q "open_or_create" "$STORE" && grep -q "fn hashes_by_path(" "$REPO" && ! grep -rq "file_hashes" crates && grep -q "Never creates; init makes the file" "$STORE" && ! grep -q "nothing is created" "$STORE"'
check "C8 replay reproduces the log"                             'svcj replay | jq -e ".diverged_at == null"'
if [ -z "${SVC_SKIP_SELF_HOST:-}" ]; then
  # A persistent target dir keeps the dependency builds between runs; the scratch tree has
  # its own path, so its crates never collide with a workspace's.
  check "C9 the merged crate type-checks (cargo check -p svc-repo)" 'CARGO_TARGET_DIR="${SVC_CHECK_TARGET:-$HOME/.cache/svc/selfhost-check}" nice -n 10 cargo check -p svc-repo --offline -j 4'
else
  echo "SKIP  C9 the merged crate type-checks (SVC_SKIP_SELF_HOST)"
fi
echo
if [ "$fail" -eq 0 ]; then echo "0 failures"; else echo "$fail failure(s); scratch kept at $WORK"; fi
exit $fail
