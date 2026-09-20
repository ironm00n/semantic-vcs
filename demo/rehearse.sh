#!/usr/bin/env bash
# The judge's path, from a fresh clone, timed step by step: clone, cargo build -p svc,
# demo/dogfood.sh on the shipped pack, demo/sync.sh, the forge (export, serve, one curl),
# the scripted demo lines (line 12's agent inside the TUI included), the dsh overlay
# (npx pre-warm, demo/ab.sh's SKIP without a credential) and, with REHEARSE_LIVE=1 and a
# key on disk, one live dsh run. Any FAIL, panic or non-zero step is red. Says whether
# artifacts/history-store.tar.xz is fresh, lags, or is stale against demo/history.
#
#   demo/rehearse.sh [-n 3]                        # runs; the first clone builds cold, later ones warm
#   REHEARSE_FROM=<git url or dir> demo/rehearse.sh # clone from there (default: this repository's git)
#   REHEARSE_LIVE=1 demo/rehearse.sh               # add the live dsh run (~/.openrouter.key)
set -u -o pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
RUNS=1
while [ $# -gt 0 ]; do case "$1" in -n) RUNS="$2"; shift 2 ;; *) echo "unknown argument $1"; exit 2 ;; esac; done
FROM="${REHEARSE_FROM:-$(cd "$ROOT" && (jj git root 2>/dev/null || echo "$ROOT/.git"))}"
command -v git >/dev/null || { echo "git is required"; exit 2; }
command -v cargo >/dev/null || { echo "cargo is required"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
WORK="$(mktemp -d "${TMPDIR:-/tmp}/svc-rehearse.XXXXXX")"
TARGET="${REHEARSE_TARGET:-$WORK/target}"    # one target dir across runs: run 1 cold, the rest warm
red=0
now() { date +%s; }
say() { printf '%-6s %-34s %5ss  %s\n' "$1" "$2" "$3" "${4:-}"; }
# step <label> <log> <command…>: times it, PASS/FAIL by exit status, a panic in the log is a FAIL.
step() {
  local label="$1" log="$2"; shift 2
  local t0; t0=$(now)
  if "$@" >"$log" 2>&1 && ! grep -q "panicked at\|RUST_BACKTRACE" "$log"; then
    say PASS "$label" $(( $(now) - t0 ))
  else
    say FAIL "$label" $(( $(now) - t0 )) "$(grep -m1 -i "FAIL\|error\|panicked" "$log" | cut -c1-90)"
    red=$((red + 1))
  fi
}

for run in $(seq 1 "$RUNS"); do
  CLONE="$WORK/clone$run"
  echo "== rehearsal $run of $RUNS — from $FROM"
  step "git clone" "$WORK/clone$run.log" git clone -q --branch main "$FROM" "$CLONE"
  [ -d "$CLONE" ] || { echo "no clone; stopping"; break; }
  cd "$CLONE"
  export CARGO_TARGET_DIR="$TARGET"
  SVC="$TARGET/debug/svc"
  step "cargo build -p svc$([ "$run" = 1 ] && echo ' (cold)' || echo ' (warm)')" "$WORK/build$run.log" cargo build -q -p svc
  [ -x "$SVC" ] || { echo "no svc binary; stopping"; break; }
  # The pack against the bundles the clone holds: fresh, lagging, or stale.
  have="$(ls demo/history/[0-9]*.json 2>/dev/null | xargs -n1 basename | sort)"
  want="$(jq -r '.bundles[]' artifacts/history-store.json 2>/dev/null | sort)"
  if [ -z "$want" ]; then pack="absent"
  elif [ "$have" = "$want" ]; then pack="fresh"
  elif [ "$(comm -23 <(echo "$want") <(echo "$have") | wc -l)" -gt 0 ]; then pack="stale"
  else pack="lags $(comm -13 <(echo "$want") <(echo "$have") | wc -l) bundle(s)"; fi
  echo "pack  artifacts/history-store.tar.xz: $pack ($(jq -r '.bundles|length' artifacts/history-store.json 2>/dev/null || echo 0) bundles, $(jq -r .ops artifacts/history-store.json 2>/dev/null || echo 0) ops)"
  export TMPDIR="$WORK/tmp$run"; mkdir -p "$TMPDIR"
  step "dogfood.sh --shell (the pack)" "$WORK/dogfood$run.log" env SHELL=/bin/true demo/dogfood.sh --shell "$SVC"
  grep -m1 "from artifacts\|replaying\|reusing" "$WORK/dogfood$run.log" | sed 's/^/       /'
  step "sync.sh" "$WORK/sync$run.log" demo/sync.sh "$SVC"
  step "cargo build -p svc-forge" "$WORK/forge-build$run.log" cargo build -q -p svc-forge
  forge_once() {
    local dir; dir="$(mktemp -d "$TMPDIR/forge.XXXXXX")"
    cp -r demo/config/. "$dir/" && rm -rf "$dir/.git" "$dir/.svc"
    (cd "$dir" && "$SVC" init --json >/dev/null && "$SVC" rename --entity parse --new-name parse_config --json >/dev/null && "$SVC" forge export --json >/dev/null) || return 1
    local port=$((20000 + RANDOM % 20000))
    "$TARGET/debug/svc-forge" --catalog "$dir/.svc/forge.json" --bind "127.0.0.1:$port" >/dev/null 2>&1 &
    local pid=$!
    local slug=""
    for _ in $(seq 1 40); do slug="$(curl -sf "http://127.0.0.1:$port/api/repositories" 2>/dev/null | jq -r '.[0].slug' 2>/dev/null)" && [ -n "$slug" ] && break; sleep 0.25; done
    local ops; ops="$(curl -sf "http://127.0.0.1:$port/api/repositories/$slug/operations" | jq length)"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    [ -n "$slug" ] && [ "$slug" != null ] && [ "${ops:-0}" -ge 2 ] && echo "served $slug: $ops operations"
  }
  step "forge export + serve + curl" "$WORK/forge$run.log" forge_once
  step "demo-lines.sh (lines 1-12, line 12 = agent in the TUI)" "$WORK/lines$run.log" demo/demo-lines.sh "$SVC"
  step "npx dsh --version (pre-warms the cache)" "$WORK/npx$run.log" timeout 120 npx -y @deepseek-ai/dsh@0.1.5-rc.2 --version
  ab_skip() {
    env -u OPENROUTER_API_KEY -u ANTHROPIC_API_KEY -u OPENAI_API_KEY -u DEEPSEEK_API_KEY -u XAI_API_KEY demo/ab.sh "$SVC" | tee "$WORK/ab$run.inner" || return 1
    grep -q "SKIP" "$WORK/ab$run.inner"
  }
  step "ab.sh without a credential (SKIP, exit 0)" "$WORK/ab$run.log" ab_skip
  if [ -n "${REHEARSE_LIVE:-}" ] && [ -f "$HOME/.openrouter.key" ]; then
    # A model that narrates its tool calls instead of making them leaves the ours log empty:
    # that is a FAIL here, not a pass on exit status.
    live() {
      OPENROUTER_API_KEY="$(cat "$HOME/.openrouter.key")" SVC_MODEL="${SVC_MODEL:-deepseek/deepseek-chat}" timeout 600 demo/ab.sh "$SVC" | tee "$WORK/live$run.inner" || return 1
      local ops; ops="$(sed -n '/^ours log:$/,$p' "$WORK/live$run.inner" | sed '1d;/^scratch:/,$d' | jq length 2>/dev/null)"
      echo "ours log: ${ops:-0} op(s)"
      [ "${ops:-0}" -ge 1 ]
    }
    step "ab.sh live (deepseek-chat via OpenRouter)" "$WORK/live$run.log" live
    ls demo/recordings/*.jsonl 2>/dev/null | sed 's/^/       recording: /' | tail -2
  else
    say SKIP "ab.sh live" 0 "REHEARSE_LIVE=1 and ~/.openrouter.key to run it"
  fi
  cd "$ROOT"
  echo
done
if [ "$red" -eq 0 ] && [ -z "${REHEARSE_LIVE:-}" ]; then echo "rehearse: 0 failures ($RUNS run(s); scratch removed)"; rm -rf "$WORK"; else echo "rehearse: $red failure(s); logs in $WORK"; fi
exit "$red"
