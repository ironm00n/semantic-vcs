#!/usr/bin/env bash
# Drop into a scratch copy of the demo crate with `svc` on PATH and the scripted agent
# wired up, for trying the product by hand. Nothing here touches the repository.
#
#   cargo build -p svc && demo/play.sh          # a fresh scratch tree, then a shell in it
#   demo/play.sh --tui                          # same, but open the review UI straight away
#   demo/play.sh --agent                        # same, with the replay agent running line 9 inside the UI
#   demo/play.sh --merge                        # same, stopped right before `svc merge a6`: run the
#                                               # binding-conflict merge yourself, then resolve it
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR}/debug/svc" ]; then
  SVC="${CARGO_TARGET_DIR}/debug/svc"
else
  SVC="$HERE/../target/debug/svc"
fi
[ -x "$SVC" ] || { echo "no svc binary at $SVC (run: cargo build -p svc)"; exit 2; }
WORK="$(mktemp -d /tmp/svc-play.XXXXXX)"
cp -r "$HERE/config/." "$WORK/" && rm -rf "$WORK/.git" "$WORK/.svc"
cd "$WORK"
"$SVC" init --json >/dev/null && "$SVC" new --json >/dev/null

item() { sed -n "/^fn $1(/,/^}/p" src/main.rs; }

# init+new leaves an empty log: every entity is only "added". Seed the SPEC
# story so the TUI opens on load with a rename, a clean merge, and a binding
# conflict in the queue. --agent keeps a virgin tree for the line-9 recording.
seed_story() {
  echo "seeding rename / merge / binding conflict…"
  "$SVC" rename --entity parse --new-name parse_config --json >/dev/null
  "$SVC" new --json >/dev/null
  "$SVC" branch a --json >/dev/null
  "$SVC" rename --entity parse_config --new-name parse_cfg --json >/dev/null
  "$SVC" branch b --json >/dev/null
  MAIN_B="$(item main | sed 's/    match load(&path) {/    let _ = parse_config("x");\n    match load(\&path) {/')"$'\n'
  "$SVC" edit-def --entity main --intent feature --definition "$MAIN_B" --json >/dev/null
  "$SVC" merge a --json >/dev/null
  "$SVC" new --json >/dev/null
  LOAD_A="$(item load | sed 's/    let cfg = parse_cfg(\&raw)?;/    let raw = normalize(\&raw);\n    let cfg = parse_cfg(\&raw)?;/')"$'\n'
  LOAD_B="$(item load | sed 's/    Ok(cfg)/    log(\&raw);\n    Ok(cfg)/')"$'\n'
  "$SVC" branch a6 --json >/dev/null
  "$SVC" edit-def --entity load --intent feature --definition "$LOAD_A" --json >/dev/null
  "$SVC" branch b6 --json >/dev/null
  "$SVC" edit-def --entity load --intent feature --definition "$LOAD_B" --json >/dev/null
  [ "${1:-}" = "--merge" ] && return 0
  "$SVC" merge a6 --json >/dev/null
}

export PATH="$(dirname "$SVC"):$PATH"
export SVC_AGENT_COMMAND="node $HERE/replay-agent.mjs $HERE/recordings/line9.ops.jsonl"
TASK="rename read to read_file and pull the retry check out of validate into its own fn"

if [ "${1:-}" != "--agent" ]; then
  seed_story "${1:-}" || echo "seed failed (playground still usable as init+new)"
fi
if [ "${1:-}" = "--merge" ]; then
  cat <<EOF
svc playground: $WORK   — two branches of \`load\` are ready; git would merge them clean and wrong.

  svc merge a6                    the binding conflict on \`raw\`, named with both binders
  svc conflicts                   list it again
  svc show load                   the merged text: normalize shadowed raw, log(&raw) means the original
  svc edit-def --entity load --intent fix --definition "\$(cat fixed.rs)"
                                  fix the binding (rename the shadow), then
  svc resolve 0 --take accept     record the code as it stands as the resolution
  svc log                         the merge and the resolution as two lines
  git init -q && git add -A >/dev/null && git diff --cached --stat | tail -1
                                  what git sees of the same tree

EOF
  exec "${SHELL:-bash}"
fi

cat <<EOF
svc playground: $WORK   (a copy of demo/config; \`svc init\` already run)

  seeded (except --agent): parse→parse_config→parse_cfg merged into main;
                           load has the line-6 binding conflict on \`raw\`
  svc log / svc op log     current change vs whole journal
  svc blame --entity load  added → edited → the capture
  svc conflicts            the one Binding on \`raw\`
  svc status               entities / semantic / layout, absorbs hand edits
  svc show parse_cfg       canonical stream: \$0 \$1 locals, #read⟨…⟩ entity refs
  svc undo                 one step, whole changeset
  svc tui                  opens on that story: revisions first (j/k, evolog below), e entities
                           (/ filters), o operation log, h/Esc back, tab queue, a/r, u undo, q
                           the store is shared: \`svc rename …\` from a second terminal shows within a second
  svc workspace add w2 $WORK-w2 ; (cd $WORK-w2 && svc new && svc rename --entity log --new-name log_line)
                           a second checkout, its own change; \`svc workspace list\`; \`svc merge <its change>\`
  svc tui --agent "$TASK"
                           line 9 runs inside the UI (scripted agent). For the real model:
                           DEEPSEEK_API_KEY=\$(cat ~/.deepseek.key) and unset SVC_AGENT_COMMAND;
                           p continues an ended turn; SVC_AGENT_PRESEED=1 sends the entity list first
  add --json to any verb for the machine form

EOF
case "${1:-}" in
  --tui)   exec svc tui ;;
  --agent) exec svc tui --agent "$TASK" ;;
  *)       exec "${SHELL:-bash}" ;;
esac
