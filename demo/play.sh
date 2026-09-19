#!/usr/bin/env bash
# Drop into a scratch copy of the demo crate with `svc` on PATH and the scripted agent
# wired up, for trying the product by hand. Nothing here touches the repository.
#
#   cargo build -p svc && demo/play.sh          # a fresh scratch tree, then a shell in it
#   demo/play.sh --tui                          # same, but open the review UI straight away
#   demo/play.sh --agent                        # same, with the replay agent running line 9 inside the UI
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
SVC="$HERE/../target/debug/svc"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (run: cargo build -p svc)"; exit 2; }
WORK="$(mktemp -d /tmp/svc-play.XXXXXX)"
cp -r "$HERE/config/." "$WORK/" && rm -rf "$WORK/.git" "$WORK/.svc"
cd "$WORK"
"$SVC" init --json >/dev/null && "$SVC" new --json >/dev/null

export PATH="$(dirname "$SVC"):$PATH"
export SVC_AGENT_COMMAND="node $HERE/replay-agent.mjs $HERE/recordings/line9.ops.jsonl"
TASK="rename read to read_file and pull the retry check out of validate into its own fn"

cat <<EOF
svc playground: $WORK   (a copy of demo/config; \`svc init\` already run)

  svc status                                   entities / semantic / layout, absorbs hand edits
  svc show parse                               canonical stream: \$0 \$1 locals, #read⟨1f2a⟩ entity refs
  svc rename --entity parse --new-name parse_config ; svc log ; grep parse_config src/main.rs
  svc new ; svc branch a ; svc rename --entity read --new-name read_file ; svc branch b
  svc edit-def --entity main --intent feature --definition "\$(cat main.rs)"   # then: svc merge a
  svc log / svc op log / svc heads / svc blame --entity load / svc evolog <change>
  svc undo                                     one step, whole changeset
  svc tui                                      review UI: j/k, tab, enter, a/r, u, q
  svc tui --agent "$TASK"
                                               line 9 runs inside the UI (scripted agent; set
                                               OPENROUTER_API_KEY and unset SVC_AGENT_COMMAND for the real model)
  add --json to any verb for the machine form

EOF
case "${1:-}" in
  --tui)   exec svc tui ;;
  --agent) exec svc tui --agent "$TASK" ;;
  *)       exec "${SHELL:-bash}" ;;
esac
