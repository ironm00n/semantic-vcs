#!/usr/bin/env bash
# PLAN.dogfood M1: svc init at the repo root over the whole tracked tree
# (Rust, .mjs, .sh, .toml, .md, .nix, images, recordings), not crates/** only.
# Proof: render is byte-identical, status 0/0, cargo test --workspace and
# node --test tests/ still pass from the rendered tree. Timings printed.
#
#   cargo build -p svc && demo/selfhost-full.sh [path/to/svc]
#   SVC_SKIP_SELFHOST_FULL=1 demo/run.sh    # skip this line
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
SVC="${1:-$ROOT/target/debug/svc}"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
SVC="$(cd "$(dirname "$SVC")" && pwd)/$(basename "$SVC")"
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
command -v cargo >/dev/null || { echo "cargo is required"; exit 2; }
command -v rsync >/dev/null || { echo "rsync is required"; exit 2; }
command -v git >/dev/null || { echo "git is required"; exit 2; }

WORK="$(mktemp -d /tmp/svc-selfhost-full.XXXXXX)"
REPO="$WORK/repo"
ORIG="$WORK/orig"
TARGET="${SVC_SELFHOST_FULL_TARGET:-${SVC_SELFHOST_TARGET:-$ROOT/target/selfhost-full}}"
mkdir -p "$TARGET"
TARGET="$(cd "$TARGET" && pwd)"
cleanup() {
  local result=$?
  if [ "$result" -eq 0 ]; then rm -rf -- "$WORK"; else echo "failed scratch retained at $WORK" >&2; fi
}
trap cleanup EXIT
fail=0
check() {
  if eval "$2" >/dev/null 2>&1; then echo "PASS  $1"; else echo "FAIL  $1"; fail=$((fail + 1)); fi
}
svcj() { "$SVC" "$@" --json; }

echo "== selfhost-full 1: copy of the whole repo (not crates/** only)"
mkdir -p "$REPO" "$ORIG"
copy_args=(-a --no-times)
if [ -d "$ROOT/.jj" ]; then
  (cd "$ROOT" && jj file list --color never -T 'path ++ "\0"') >"$WORK/files"
  copy_args+=(--from0 "--files-from=$WORK/files")
elif [ -e "$ROOT/.git" ]; then
  git -C "$ROOT" ls-files -z >"$WORK/files"
  copy_args+=(--from0 "--files-from=$WORK/files")
else
  echo "      no VCS metadata: checking source-directory contents, excluding generated directories"
  copy_args+=(--exclude .git/ --exclude .jj/ --exclude target/ --exclude .svc/ --exclude node_modules/ --exclude .svc-workspace)
fi
rsync "${copy_args[@]}" "$ROOT"/ "$REPO/"
# Scratch git/jj must ignore their own metadata or 3b/3c go red after init.
rsync -a "$REPO"/ "$ORIG/"
# git index so `git diff --stat` is the byte-identical check (scratch is not the team's jj WC).
(
  cd "$REPO"
  git init -q
  printf '/.jj/\n/.svc/\n/.svc-workspace\n' >> .git/info/exclude
  git add -A
  git -c user.email=me@ironmoon.dev -c user.name=ironmoon commit -qm seed
)
if command -v jj >/dev/null; then
  (cd "$REPO" && jj git init --colocate >/dev/null 2>&1)
fi
n_files="$(find "$REPO" -type f ! -path '*/.git/*' ! -path '*/.jj/*' | wc -l)"
check "1  full tree copied ($n_files files)" 'test "$n_files" -gt 80'

echo "== selfhost-full 2: svc init at the repo root"
cd "$REPO"
start=$(date +%s%3N)
svcj init >"$WORK/init.json"
s2="$(svcj status)"
init_ms=$(( $(date +%s%3N) - start ))
echo "time  init+status ${init_ms}ms"
check "2a init: N entities, 0/0, clean" 'echo "$s2" | jq -es "length == 1 and (.[0] | .entities > 0 and .semantic == 0 and .layout == 0 and .clean == true and .absorbed == false)"'
n_entities="$(echo "$s2" | jq -r '.entities')"
echo "      $n_entities entities"

echo "== selfhost-full 3: render is byte-identical ($n_entities entities)"
start=$(date +%s%3N)
svcj render >"$WORK/render.json"
echo "time  render $(( $(date +%s%3N) - start ))ms"
check "3a git diff --stat empty" 'git diff --stat --exit-code'
check "3b git status porcelain empty" 'test -z "$(git status --porcelain)"'
if command -v jj >/dev/null && [ -d .jj ]; then
  check "3c jj status clean" 'jj status > "$WORK/jj-status" && grep -q "The working copy has no changes." "$WORK/jj-status"'
else
  echo "SKIP  3c jj status (scratch has no jj colocate)"
fi
# Also against the pre-init snapshot: nothing but .svc/ and .git/
check "3d tree matches the pre-init copy except .svc/.git/.jj" 'diff -rq --exclude .svc --exclude .git --exclude .jj "$ORIG" "$REPO"'
FROM_STORE="$WORK/from-store"
svcj workspace add full-tree "$FROM_STORE" >"$WORK/workspace.json"
# What .svcignore names never entered the store, so an empty checkout cannot render it.
if [ -f "$ORIG/.svcignore" ]; then
  sed -e 's/#.*//' -e 's/ *$//' -e '/^$/d' "$ORIG/.svcignore" | while read -r p; do
    case "$p" in */*) rm -rf "$ORIG/$p" ;; *) find "$ORIG" -name "$p" -prune -exec rm -rf {} + ;; esac
  done
fi
check "3e empty checkout reproduces every source file" 'diff -rq --exclude .svc --exclude .svc-workspace "$ORIG" "$FROM_STORE"'
[ "$fail" -eq 0 ] || exit "$fail"
cd "$FROM_STORE"

echo "== selfhost-full 4: cargo test --workspace from the rendered tree"
start=$(date +%s%3N)
if (cd "$FROM_STORE" && CARGO_TARGET_DIR="$TARGET" cargo test --workspace --offline --quiet) >"$WORK/cargo-test.log" 2>&1; then
  echo "PASS  4  cargo test --workspace"
else
  echo "FAIL  4  cargo test --workspace"
  tail -40 "$WORK/cargo-test.log"
  fail=$((fail + 1))
fi
echo "time  cargo test $(( $(date +%s%3N) - start ))ms"

echo "== selfhost-full 5: node --test tests/"
start=$(date +%s%3N)
if ! command -v node >/dev/null; then
  echo "FAIL  5  node is required"
  fail=$((fail + 1))
elif find tests -type f \( -name '*.js' -o -name '*.mjs' -o -name '*.cjs' \) -print -quit | grep -q .; then
  if (cd "$FROM_STORE" && node --test tests/) >"$WORK/node-test.log" 2>&1; then
    echo "PASS  5  node --test tests/"
  else
    echo "FAIL  5  node --test tests/"
    tail -40 "$WORK/node-test.log"
    fail=$((fail + 1))
  fi
else
  echo "SKIP  5  tests/ has no .js/.mjs (muse O9-JS is M1's JS half)"
fi
echo "time  node --test $(( $(date +%s%3N) - start ))ms"

echo "== selfhost-full 6: status still 0/0"
start=$(date +%s%3N)
s6="$(svcj status)"
echo "time  status $(( $(date +%s%3N) - start ))ms"
check "6  0 semantic / 0 layout / clean" 'echo "$s6" | jq -es "length == 1 and (.[0] | .semantic == 0 and .layout == 0 and .clean == true and .absorbed == false)"'

echo
echo "$fail failure(s)"
exit $fail
