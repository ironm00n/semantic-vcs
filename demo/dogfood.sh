#!/usr/bin/env bash
# Build a real review story from svc's own source, then optionally open its TUI.
#
#   cargo build -p svc
#   demo/dogfood.sh                         # prepare, verify, and print the scratch path
#   demo/dogfood.sh --tui                   # open the revision-first review UI
#   demo/dogfood.sh --shell                 # enter the prepared checkout
#   demo/dogfood.sh --agent "task"          # run an agent inside the review UI
#   demo/dogfood.sh --tui path/to/svc
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
MODE=check
TASK="Using svc tools only, rename describe_op to describe_operation. Do not edit files directly."

case "${1:-}" in
  --tui|--shell)
    MODE="${1#--}"
    shift
    ;;
  --agent)
    MODE=agent
    shift
    if [ -n "${1:-}" ] && [ "${1#-}" = "$1" ] && [ ! -x "$1" ]; then
      TASK="$1"
      shift
    fi
    ;;
esac

SVC="${1:-$ROOT/target/debug/svc}"
SVC="$(cd "$(dirname "$SVC")" && pwd)/$(basename "$SVC")"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (run: cargo build -p svc)"; exit 2; }
command -v cargo >/dev/null || { echo "cargo is required"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }

WORK="$(mktemp -d /tmp/svc-dogfood.XXXXXX)"
REPO="$WORK/repo"
INTEGRATION="$WORK/integration"
TARGET="${SVC_DOGFOOD_TARGET_DIR:-$ROOT/target/dogfood-demo}"
mkdir -p "$REPO"
cp -r "$ROOT/crates" "$ROOT/harness" "$REPO/"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$REPO/"

fail=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; fail=$((fail + 1)); }
check() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then pass "$label"; else fail "$label"; fi
}
check_json() {
  local label="$1" json="$2" query="$3"
  if jq -e "$query" >/dev/null 2>&1 <<<"$json"; then pass "$label"; else fail "$label"; fi
}
svcj() { "$SVC" "$@" --json; }
build() { (cd "$1" && CARGO_TARGET_DIR="$TARGET" cargo check --workspace --offline --quiet); }
current_change() { svcj heads | jq -r '.[] | select(.current) | .change'; }

cd "$REPO"
echo "== dogfood 1: import svc's real Rust crates and agent harness"
init="$(svcj init)"
status="$(svcj status)"
check_json "1a init imported the multi-crate workspace" "$status" '.entities > 1000 and .semantic == 0 and .layout == 0 and .clean'
check "1b the untouched imported workspace compiles" build "$REPO"
svcj describe "Baseline: svc imports and builds itself" >/dev/null
BASE="$(current_change)"


echo "== dogfood 2: a named content-engine change"
svcj branch content >/dev/null
svcj changeset begin "content terminology" --intent refactor >/dev/null
svcj rename --entity coalesce_literals --new-name coalesce_adjacent_literals >/dev/null
svcj changeset end >/dev/null
svcj describe "Name literal coalescing explicitly" >/dev/null
CONTENT="$(current_change)"
check "2a semantic rename rewrote the real definition and callers" sh -c 'grep -q "fn coalesce_adjacent_literals" crates/svc-core/src/content.rs && ! grep -q "coalesce_literals" crates/svc-core/src/content.rs'
check "2b renamed svc-core compiles" env CARGO_TARGET_DIR="$TARGET" cargo check -p svc-core --offline --quiet


echo "== dogfood 3: a parallel review-UI change from the same baseline"
svcj edit "$BASE" >/dev/null
svcj branch review-ui >/dev/null
svcj changeset begin "review UI terminology" --intent refactor >/dev/null
svcj rename --entity kind_glyph --new-name entity_kind_glyph >/dev/null
svcj changeset end >/dev/null
svcj describe "Clarify entity glyph rendering" >/dev/null
REVIEW="$(current_change)"
check "3a the parallel change starts from the baseline" sh -c 'grep -q "fn coalesce_literals" crates/svc-core/src/content.rs && grep -q "fn entity_kind_glyph" crates/svc-tui/src/data.rs'
check "3b changed review UI compiles" env CARGO_TARGET_DIR="$TARGET" cargo check -p svc-tui --offline --quiet


echo "== dogfood 4: merge the parallel refactors through a named checkout"
svcj workspace add integration "$INTEGRATION" --at "$REVIEW" >/dev/null
cd "$INTEGRATION"
merge="$(svcj merge content)"
check_json "4a independent full-source changes merge without conflict" "$merge" '.conflicts | length == 0'
check "4b both semantic renames render in the integration checkout" sh -c 'grep -q "fn coalesce_adjacent_literals" crates/svc-core/src/content.rs && grep -q "fn entity_kind_glyph" crates/svc-tui/src/data.rs'


echo "== dogfood 5: reviewed edit-def on the integrated change"
shown="$(svcj show-def --entity plain_lines)"
plain="$(jq -r 'if (.text // "") != "" then .text else (.bytes.src | implode) end' <<<"$shown")"
edited="$(sed -e 's/let mut lines =/let mut rendered =/' -e 's/if lines\.is_empty()/if rendered.is_empty()/' -e 's/lines\.push(/rendered.push(/g' -e 's/^    lines$/    rendered/' <<<"$plain")"
[ "$edited" != "$plain" ] || { echo "plain_lines fixture no longer matches"; exit 2; }
svcj changeset begin "review plaintext fallback" --intent refactor >/dev/null
svcj edit-def --entity plain_lines --intent refactor --definition "$edited" >/dev/null
svcj changeset end >/dev/null
svcj describe "Integrate content and review UI dogfood" >/dev/null
INTEGRATED="$(current_change)"
ops="$(svcj op log)"
sets="$(svcj changeset list)"
check_json "5a edit-def is observed alpha-only and unflagged" "$ops" '[.[] | select((.op | type) == "object" and (.op | has("EditDef")))][0] | .observed == "Alpha" and (.flagged | not)'
check_json "5b the review unit is one closed changeset" "$sets" '.[] | select(.name == "review plaintext fallback") | (.open | not) and (.ops | length == 1)'
check "5c the complete integrated workspace compiles" build "$INTEGRATION"


echo "== dogfood 6: history, replay, workspaces, and forge are all live"
heads="$(svcj heads)"
replay="$(svcj replay)"
workspace_list="$(svcj workspace list)"
forge="$(svcj forge export)"
final_status="$(svcj status)"
check_json "6a revision view has baseline, parallel changes, and integration" "$heads" 'length >= 4 and ([.[].message] | index("Name literal coalescing explicitly") != null) and ([.[].message] | index("Clarify entity glyph rendering") != null) and ([.[].message] | index("Integrate content and review UI dogfood") != null)'
check_json "6b replay reproduces the current store" "$replay" '.diverged_at == null and .ops >= 10'
check_json "6c both working copies share the store" "$workspace_list" 'length >= 2'
check_json "6d forge export contains the dogfood history" "$forge" '.path | endswith(".svc/forge.json")'
check_json "6e final working copy is clean" "$final_status" '.clean and .semantic == 0 and .layout == 0'


echo
echo "revision graph prepared from svc's own source:"
"$SVC" heads
echo
echo "dogfood checkout: $INTEGRATION"
echo "shared target:    $TARGET"
echo "changes: baseline=$BASE content=$CONTENT review-ui=$REVIEW integrated=$INTEGRATED"
echo "TUI keys: j/k revisions, e entities, / filter, o oplog, h/Esc revisions, tab queue, q quit"
echo
if [ "$fail" -ne 0 ]; then
  echo "$fail failure(s); scratch preserved at $WORK"
  exit "$fail"
fi
echo "0 failures; scratch preserved at $WORK"

export PATH="$(dirname "$SVC"):$PATH"
case "$MODE" in
  tui) exec "$SVC" tui ;;
  shell) exec "${SHELL:-bash}" ;;
  agent) exec "$SVC" tui --agent "$TASK" ;;
esac
