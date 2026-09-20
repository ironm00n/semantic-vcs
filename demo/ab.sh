#!/usr/bin/env bash
# A/B: same agent task under stock dsh vs overlay dsh (SPEC lane demo, F11/F12).
# Two separate processes. Working copies reset from demo/pristine between arms.
# Stock arm has no .svc/ so file writes cannot be absorbed.
#
#   cargo build -p svc && demo/ab.sh [path-to-svc]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
PRISTINE="$HERE/pristine"
SVC="${1:-$ROOT/target/debug/svc}"
if [ -n "${1:-}" ]; then
  SVC="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
fi
DSH="${DSH_BIN:-npx -y @deepseek-ai/dsh@0.1.5-rc.2}"
TASK='In src/main.rs, using svc tools only: (1) rename entity read to read_file. (2) add_def after load a complete fn check_retries(c: &Config) -> Result<(), Error> that returns Err if c.retries > 10, otherwise Ok(()). (3) edit_def validate to the complete item fn validate(c: &Config) -> Result<(), Error> { check_retries(c)?; Ok(()) }. check_retries is new so do not extract. Do not stub or comment-only bodies. Do not edit files.'

has_key=0
for v in OPENROUTER_API_KEY ANTHROPIC_API_KEY OPENAI_API_KEY DEEPSEEK_API_KEY XAI_API_KEY; do
  eval "val=\${$v:-}"
  if [ -n "$val" ]; then has_key=1; break; fi
done

stage() {
  local dest="$1" with_svc="$2"
  rm -rf "$dest"
  mkdir -p "$dest"
  cp -a "$PRISTINE/." "$dest/"
  rm -rf "$dest/.git" "$dest/.svc" "$dest/.jj" "$dest/target"
  if [ "$with_svc" = yes ]; then
    (cd "$dest" && "$SVC" init --json >/dev/null)
  fi
}

if [ ! -f "$PRISTINE/src/main.rs" ]; then
  echo "FAIL  demo/pristine/src/main.rs missing"
  exit 1
fi

echo "== asserting pristine trees are byte-identical before either arm"
STOCK="$(mktemp -d)/stock"
OURS="$(mktemp -d)/ours"
stage "$STOCK" no
stage "$OURS" no
if ! diff -rq "$STOCK" "$OURS" >/dev/null; then
  echo "FAIL  starting trees differ"
  exit 1
fi
echo "PASS  starting trees identical"

if [ "$has_key" -eq 0 ]; then
  echo
  echo "SKIP live A/B: no model credential in the environment."
  echo "After credentials land, re-run this script. Arms would be:"
  echo
  echo "  # stock (no .svc, no overlay — two separate dsh processes, F11)"
  echo "  (cd $STOCK && $DSH --profile acp)"
  echo
  echo "  # ours (svc repo + overlay; plugin path patched at agent boot)"
  echo "  (cd $OURS && $SVC init && $SVC agent \"$TASK\")"
  echo
  echo "Scripted stand-in for the ours arm is demo/demo-lines.sh line 9;"
  echo "transcript: demo/recordings/line9.jsonl"
  exit 0
fi

stage "$STOCK" no
echo "== stock arm (headless dsh, stock tools, no .svc)"
# The same provider and model as the overlay (its llm-pi-ai and agent-default-model
# layers, nothing else), so the two arms differ only in the tool set; SVC_MODEL swaps
# the model as it does for the ours arm. The acp profile speaks JSON-RPC on stdin; the
# headless profile takes the task as an argument and answers once.
PATCH="$(mktemp "${TMPDIR:-/tmp}/svc-stock.XXXXXX.yml")"
awk '/^- id: llm-pi-ai/{p=1} /^- id: acp/{p=0} /^- id: agent-default-model/{p=1} /^- id: session-log-deepseek/{p=0} p' "$HERE/../harness/overlay.yml" \
  | sed "s#deepseek/deepseek-chat#${SVC_MODEL:-deepseek/deepseek-chat}#" > "$PATCH"
# shellcheck disable=SC2086
# Stock has no svc tools, so its task says what to change, not how: the same three edits.
STOCK_TASK="$(printf '%s' "$TASK" | sed 's/, using svc tools only:/:/; s/ Do not edit files\./ Edit src\/main.rs directly./; s/rename entity read/rename the function read/; s/add_def after load/add after load/; s/edit_def validate to/change validate to/')"
(cd "$STOCK" && $DSH --profile headless --patch "$PATCH" "$STOCK_TASK")
rm -f "$PATCH"
echo "stock tree:"
find "$STOCK" -type f ! -path '*/target/*' | sort
echo "stock diff against pristine (git's view of the same task):"
diff -u "$PRISTINE/src/main.rs" "$STOCK/src/main.rs" | grep -c "^[-+][^-+]" | sed 's/^/  changed lines: /'

stage "$OURS" yes
echo "== ours arm (svc overlay via svc agent)"
(cd "$OURS" && "$SVC" agent "$TASK")
echo "ours log:"
(cd "$OURS" && "$SVC" log --json)
echo "scratch: stock=$STOCK ours=$OURS"
