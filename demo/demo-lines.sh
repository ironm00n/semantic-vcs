#!/usr/bin/env bash
# Scripted run of SPEC §10 demo lines 1–12 against the built `svc` binary, on a scratch
# copy of demo/config. Prints one PASS/FAIL per observable; exit status is the number of failures.
#
#   cargo build -p svc && demo/demo-lines.sh [path/to/svc]
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
SVC="${1:-$HERE/../target/debug/svc}"
[ -x "$SVC" ] || { echo "no svc binary at $SVC (cargo build -p svc)"; exit 2; }
command -v jq >/dev/null || { echo "jq is required"; exit 2; }
WORK="$(mktemp -d)"
cp -r "$HERE/config/." "$WORK/" && rm -rf "$WORK/.git" "$WORK/.svc"
cd "$WORK"
fail=0
check() { # check <label> '<shell expression>'
  if eval "$2" >/dev/null 2>&1; then echo "PASS  $1"; else echo "FAIL  $1"; fail=$((fail + 1)); fi
}
svcj() { "$SVC" "$@" --json; }
item() { sed -n "/^fn $1(/,/^}/p" src/main.rs; }
jsitem() { sed -n "/^export function $1(/,/^}/p" src/config.js; }

echo "== line 1: init + status"
svcj init >/dev/null
s1="$(svcj status)"
check "1  N entities, 0 changes"          'echo "$s1" | jq -e ".entities > 0 and .semantic == 0 and .layout == 0"'

echo "== line 2: rename a local in parse → layout only"
sed -i '/^fn parse(/,/^}/ s/\bs\b/text/g' src/main.rs
s2="$(svcj status)"
check "2  no semantic changes; 1 layout"   'echo "$s2" | jq -e ".semantic == 0 and .layout == 1"'
svcj new >/dev/null

echo "== line 3: show parse"
s3="$("$SVC" show parse 2>&1)"
check "3  canonical stream with slots"     'echo "$s3" | grep -q "\$0" && echo "$s3" | grep -q "⟨"'

echo "== line 4: rename parse → parse_config"
svcj rename --entity parse --new-name parse_config >/dev/null
l4="$(svcj log)"
check "4a one event: renamed"              'echo "$l4" | jq -e "length == 1 and (.[0].op | has(\"Rename\"))"'
check "4b callers render the new name"     'grep -q "parse_config(&raw)" src/main.rs && ! grep -q "parse(&raw)" src/main.rs'
svcj new >/dev/null

echo "== line 5: branch a (rename) / branch b (edit main) / merge a"
svcj branch a >/dev/null
svcj rename --entity parse_config --new-name parse_cfg >/dev/null
svcj branch b >/dev/null
MAIN_B="$(item main | sed 's/    match load(&path) {/    let _ = parse_config("x");\n    match load(\&path) {/')"$'\n'
svcj edit-def --entity main --intent feature --definition "$MAIN_B" >/dev/null
m5="$(svcj merge a)"
check "5a merge is clean"                  'echo "$m5" | jq -e ".conflicts | length == 0"'
check "5b rendered main calls parse_cfg"   'grep -q "let _ = parse_cfg(\"x\")" src/main.rs'

echo "== line 6: both branches edit load → binding conflict (git merges clean and wrong)"
svcj new >/dev/null
LOAD_A="$(item load | sed 's/    let cfg = parse_cfg(&raw)?;/    let raw = normalize(\&raw);\n    let cfg = parse_cfg(\&raw)?;/')"$'\n'
LOAD_B="$(item load | sed 's/    Ok(cfg)/    log(\&raw);\n    Ok(cfg)/')"$'\n'
svcj branch a6 >/dev/null
svcj edit-def --entity load --intent feature --definition "$LOAD_A" >/dev/null
svcj branch b6 >/dev/null
svcj edit-def --entity load --intent feature --definition "$LOAD_B" >/dev/null
m6="$(svcj merge a6)"
check "6a exactly one conflict, on load"   'echo "$m6" | jq -e ".conflicts | length == 1 and .[0].name == \"load\" and (.[0].conflict | has(\"Binding\"))"'
check "6b it names raw with both targets"  'echo "$m6" | jq -e ".conflicts[0].conflict.Binding | (.was != .now)"'
check "6c merged load has both edits"      'item load | grep -q "normalize(&raw)" && item load | grep -q "log(&raw)"'

echo "== line 7: edit-def validate declared refactor, shadows c → flagged"
svcj new >/dev/null
VAL7="$(item validate | sed 's/    if c.retries > 10 {/    let c = \&canon(c);\n    if c.retries > 10 {/')"$'\n'
svcj edit-def --entity validate --intent refactor --definition "$VAL7" >/dev/null
l7="$(svcj log)"
check "7  declared refactor / observed binding-changing / flagged" 'echo "$l7" | jq -e ".[0].declared == \"Refactor\" and .[0].observed == \"BindingChanging\" and .[0].flagged"'
svcj undo >/dev/null
echo "== line 8: JS twin (config-js) — init/status/alpha/show/rename/merge"
JSWORK="$(mktemp -d)"
cp -r "$HERE/config-js/." "$JSWORK/" && rm -rf "$JSWORK/.svc"
cd "$JSWORK"
j1="$(svcj init >/dev/null; svcj status)"
check "8a JS init: entities, 0 changes"  'echo "$j1" | jq -e ".entities > 0 and .semantic == 0 and .layout == 0"'
sed -i 's/export function read(path)/export function read(input)/; s/readFileSync(path,/readFileSync(input,/' src/config.js
j2="$(svcj status)"
check "8b JS local rename is layout-only" 'echo "$j2" | jq -e ".semantic == 0"'
svcj new >/dev/null
j3="$("$SVC" show read 2>&1)"
check "8c JS canonical stream has slots"  'echo "$j3" | grep -q "\$0"'
svcj rename --entity read --new-name read_file >/dev/null
check "8d JS rename propagates to caller" 'grep -q "read_file(path)" src/config.js && ! grep -q "[ (]read(path)" src/config.js'
svcj new >/dev/null
svcj branch js-a >/dev/null
svcj rename --entity read_file --new-name read_cfg >/dev/null
svcj branch js-b >/dev/null
JS_LOAD_B="$(jsitem load | sed 's/  const raw = read_file(path)/  const raw = read_file(path)\n  void read_file(path)/')"$'\n'
svcj edit-def --entity load --intent feature --definition "$JS_LOAD_B" >/dev/null
jm="$(svcj merge js-a)"
check "8e JS rename/add-call merge is clean" 'echo "$jm" | jq -e ".conflicts | length == 0"'
check "8f JS merged calls follow rename" 'test "$(grep -c "read_cfg(path)" src/config.js)" -eq 2 && ! grep -q "read_file(path)" src/config.js'
cd "$WORK"

echo "== line 9 (scripted stand-in for the agent): three ops in one changeset"
svcj new >/dev/null
svcj changeset begin "agent run" >/dev/null
svcj rename --entity read --new-name read_file >/dev/null
svcj add-def --ordinal 9 --intent refactor --definition 'fn check_retries(c: &Config) -> Result<(), Error> {
    if c.retries > 10 {
        return Err(Error("too many retries".into()));
    }
    Ok(())
}' >/dev/null
VAL9="$(item validate | sed 's/    if c.retries > 10 {/    check_retries(c)?;\n    if false {/')"$'\n'
svcj edit-def --entity validate --intent refactor --definition "$VAL9" >/dev/null
svcj changeset end >/dev/null
l9="$(svcj log)"
check "9a exactly three events"            'echo "$l9" | jq -e "length == 3"'
check "9b renamed, add-def, edit-def ✓"    'echo "$l9" | jq -e "(.[2].op | has(\"Rename\")) and (.[1].op | has(\"AddDef\")) and (.[0].op | has(\"EditDef\")) and .[0].observed == \"BindingPreserving\" and (.[0].flagged | not)"'
check "9c rename propagated to load"       'grep -q "read_file(path)" src/main.rs'

echo "== line 10: evolog after amending"
ch="$(svcj heads | jq -r ".[0].change")"
e10="$(svcj evolog "$ch")"
check "10 evolog has entries with deltas"  'echo "$e10" | jq -e "length >= 2 and (.[0].deltas | length > 0)"'

echo "== line 11: undo the whole changeset"
svcj undo >/dev/null
check "11a back to the pre-agent tree"     '! grep -q "read_file\|check_retries" src/main.rs'
check "11b op log shows the undo"          'svcj op log | jq -e ".[0].op == \"Undo\""'

echo "== line 12: line 9 inside the TUI — replay agent streams ops, the edit-def ask is answered from the queue"
if command -v script >/dev/null && command -v node >/dev/null; then
  svcj new >/dev/null
  export SVC_AGENT_COMMAND="node $HERE/replay-agent.mjs $HERE/recordings/line9.ops.jsonl"
  # a sized pty; `a` allows the one ask once it is up, `q` quits after the run finishes
  ( (sleep 6; printf 'a'; sleep 4; printf 'q') \
    | timeout 40 script -qfec "stty cols 150 rows 40; $SVC tui --agent 'rename read to read_file and pull the retry check out of validate'" /dev/null ) >/dev/null 2>&1
  unset SVC_AGENT_COMMAND
  cs12="$(svcj changeset list)"
  l12="$(svcj log)"
  check "12a the run is one closed changeset of three ops" 'echo "$cs12" | jq -e ".[0].open == false and (.[0].ops | length) == 3"'
  check "12b log: renamed, add-def, edit-def ✓"            'echo "$l12" | jq -e "(.[2].op | has(\"Rename\")) and (.[1].op | has(\"AddDef\")) and (.[0].op | has(\"EditDef\")) and .[0].observed == \"BindingPreserving\""'
  check "12c the agent's edits are in the tree"            'grep -q "read_file(path)" src/main.rs && grep -q "fn check_retries" src/main.rs'
  svcj undo >/dev/null
  check "12d undo reverts the whole run"                   '! grep -q "read_file\|check_retries" src/main.rs'
else
  echo "SKIP  12 (needs script(1) and node)"
fi

echo
echo "$fail failure(s); scratch tree at $WORK"
exit $fail
