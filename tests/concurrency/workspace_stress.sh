#!/usr/bin/env bash
set -euo pipefail

svc=${SVC_BIN:?set SVC_BIN to an svc executable}
clients=${1:-32}
case "$svc" in /*) ;; *) echo "SVC_BIN must be absolute" >&2; exit 2;; esac
[[ -x "$svc" ]] || { echo "SVC_BIN is not executable: $svc" >&2; exit 2; }
[[ "$clients" =~ ^[1-9][0-9]*$ ]] || { echo "clients must be positive" >&2; exit 2; }

scratch=$(mktemp -d "${TMPDIR:-/tmp}/svc-concurrency.XXXXXX")
cleanup() { rm -rf -- "$scratch"; }
trap cleanup EXIT
repo=$scratch/repo
mkdir -p "$repo/src" "$scratch/out"
printf '%s\n' 'pub fn value() -> usize { 1 }' >"$repo/src/lib.rs"
(cd "$repo" && "$svc" --json init >"$scratch/init.json")

for i in $(seq 1 "$clients"); do
  mkdir -p "$scratch/ws-$i"
  (cd "$repo" && "$svc" --json workspace add "w$i" "$scratch/ws-$i" >"$scratch/out/add-$i.json")
done
before=$(cd "$repo" && "$svc" --json op log | jq 'length')

barrier=$scratch/start
mkfifo "$barrier"
for i in $(seq 1 "$clients"); do
  (
    read -r _ <"$barrier"
    cd "$scratch/ws-$i"
    "$svc" --json new >"$scratch/out/new-$i.json" 2>"$scratch/out/new-$i.err"
  ) &
  pids[$i]=$!
done
exec 9>"$barrier"
for _ in $(seq 1 "$clients"); do printf 'go\n' >&9; done
exec 9>&-

fail=0
for i in $(seq 1 "$clients"); do
  if ! wait "${pids[$i]}"; then
    echo "client $i failed: $(<"$scratch/out/new-$i.err")" >&2
    fail=1
  fi
done
(( fail == 0 )) || exit 1

after=$(cd "$repo" && "$svc" --json op log | jq 'length')
[[ $((after - before)) -eq "$clients" ]] || {
  echo "lost op-log updates: expected +$clients, observed +$((after - before))" >&2
  exit 1
}

rows=$(cd "$repo" && "$svc" --json workspace list)
[[ $(jq 'length' <<<"$rows") -eq $((clients + 1)) ]] || {
  echo "workspace row lost under contention" >&2
  exit 1
}
for i in $(seq 1 "$clients"); do
  jq -e --arg name "w$i" '.[] | select(.name == $name) | .snapshot != null and .change != null' \
    <<<"$rows" >/dev/null || { echo "workspace w$i has no published root" >&2; exit 1; }
  (cd "$scratch/ws-$i" && "$svc" --json status >/dev/null)
done

printf 'ok: %s concurrent workspace clients, %s unique published ops, all roots reopen\n' \
  "$clients" "$clients"
