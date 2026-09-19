#!/usr/bin/env bash
# Scope item 2: svc self-hosting / dogfooding, with repeatable proof rather than a
# one-off scratch demo. Runs `svc` against a pristine copy of THIS repo's own
# `crates/**` (thousands of real lines of Rust across five real crates, not a toy
# fixture): init, build the untouched render, rename a real same-file-used entity,
# rebuild, undo, rebuild again. Every "build" is a real `cargo build` of the
# rendered tree, so a wrong byte in the render or a broken rename fails loudly.
#
# O9 (crates/svc-core/tests/o9_compiler_oracle.rs) already proves the whole crate
# survives an exhaustive alpha-rename of every local, engine-API-only. This is the
# complementary, CLI-level line: one real targeted `svc rename`, through init/
# render/undo, on the actual multi-crate source tree, gated as a repeatable line
# instead of a hidden oracle test. Then prove the store is enough: a named
# checkout into an empty directory still `cargo build`s, and `svc replay` is clean.
#
#   cargo build -p svc && demo/self-host.sh [path/to/svc]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
SVC="${1:-$ROOT/target/debug/svc}"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
command -v cargo >/dev/null || { echo "cargo is required"; exit 2; }

WORK="$(mktemp -d)"
fail=0
check() { # check <label> '<shell expression>'
  if eval "$2" >/dev/null 2>&1; then echo "PASS  $1"; else echo "FAIL  $1"; fail=$((fail + 1)); fi
}
svcj() { "$SVC" "$@" --json; }
build() { # build <label-for-log-file> [dir] -> 0 on success
  local dir="${2:-.}"
  (cd "$dir" && cargo build --offline --quiet) >"$WORK/build-$1.log" 2>&1
}

echo "== self-host 1: pristine copy of this repo's own crates/**"
mkdir -p "$WORK/repo"
cp -r "$ROOT/crates" "$WORK/repo/crates"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$WORK/repo/"
find "$WORK/repo" -name target -type d -prune -exec rm -rf {} +
cd "$WORK/repo"
n_files="$(find crates -name '*.rs' | wc -l)"
check "1  real source, $n_files .rs files across 5 crates" 'test "$n_files" -gt 50'

echo "== self-host 2: svc init tracks it, clean"
s1="$(svcj init >/dev/null; svcj status)"
check "2a init: N entities, 0 changes"  'echo "$s1" | jq -e ".entities > 0 and .semantic == 0 and .layout == 0"'
n_entities="$(echo "$s1" | jq -r '.entities')"

echo "== self-host 3: the pristine render is byte-identical enough to still build"
check "3  cargo build of the untouched render ($n_entities entities)" 'build pristine'

echo "== self-host 4: rename a real, same-file-used svc-core entity"
svcj rename --entity coalesce_literals --new-name coalesce_adjacent_literals >/tmp/self-host-rename.json 2>&1
l4="$(svcj log)"
check "4a one event: renamed"                'echo "$l4" | jq -e "length == 1 and (.[0].op | has(\"Rename\"))"'
check "4b new name rendered, old gone"       'grep -q "fn coalesce_adjacent_literals" crates/svc-core/src/content.rs && ! grep -q "coalesce_literals" crates/svc-core/src/content.rs'

echo "== self-host 5: the renamed tree — the real crate, not a fixture — still builds"
check "5  cargo build after the real rename" 'build renamed'

echo "== self-host 6: undo reverts it, on its own source"
svcj undo >/dev/null
check "6a name reverted"                     'grep -q "fn coalesce_literals" crates/svc-core/src/content.rs && ! grep -q "coalesce_adjacent_literals" crates/svc-core/src/content.rs'
check "6b op log shows the undo"             'svcj op log | jq -e ".[0].op == \"Undo\""'

echo "== self-host 7: post-undo tree builds again"
check "7  cargo build after undo" 'build post-undo'

echo "== self-host 8: the store alone is enough (empty checkout, no extra cp)"
export SVC_LOCK_TIMEOUT_MS="${SVC_LOCK_TIMEOUT_MS:-60000}"
FROM="$WORK/from-store"
svcj workspace add from-store "$FROM" >/dev/null
check "8a named checkout has Cargo.toml from the store" 'test -f "$FROM/Cargo.toml" && grep -q "\[package\]\|\[workspace\]" "$FROM/Cargo.toml"'
check "8b named checkout has the renamed-then-undone source" 'test -f "$FROM/crates/svc-core/src/content.rs" && grep -q "fn coalesce_literals" "$FROM/crates/svc-core/src/content.rs"'
check "8c cargo build of the store-only checkout" 'build from-store "$FROM"'

echo "== self-host 9: op log replays"
r9="$(svcj replay)"
check "9  replay is clean" 'echo "$r9" | jq -e ".diverged_at == null and .ops > 0"'

echo
echo "$fail failure(s); self-host scratch at $WORK (build logs: $WORK/build-*.log)"
exit $fail
