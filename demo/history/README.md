# demo/history — this repository's own changes, made through svc, kept as bundles

Every svc-made landing on `main` leaves a bundle here: the op log of the author's store
from `svc init` on (renames, typed edits, hand edits with the files they changed, merges,
resolutions, undos), with entity paths and per-op tree hashes so a fresh store over the
same tree replays it exactly. `replay.sh` rebuilds the whole history into one store, in
landing order, from git's trees; `demo/dogfood.sh --tui` opens on it.

## Recording a landing (the author, before `jj describe`)

1. Work in a checkout whose tree was clean at a `main` commit when you ran `svc init`.
   That commit is the bundle's base: write it down *then* —
   `base=100 1 100jj log -r 569 --no-graph -T "commit_id.short(8)")` — main moves while you work.
2. Make the change with svc verbs; `svc status` absorbs hand edits to opaque files.
3. `svc history export --since 1 --out demo/history/NNNN-<base git sha>-<change>.json`
   (NNNN: the next number, a hint only — replay orders bundles by how deep the base
   commit sits in history, so two agents picking the same number is harmless).
4. Land the change and the bundle together; the description carries the `svc log` lines.

## Refreshing

The engine keeps changing; a typed op replayed by a newer engine may leave a tree the
record did not. Every bundle therefore carries the files each op changed, and an import
takes the record when the engine disagrees (`from_record` in its report; `replay.sh` says
so and passes). `refresh.sh` re-cuts every bundle that replays with the current exporter,
in place — run it after an engine change, before more bundles are cut.

## Replaying

`demo/history/replay.sh <dir>` — for each bundle in order: put git's tree at the bundle's
base commit into `<dir>` (everyone else's landings in between arrive as one absorbed hand
edit, which is what they were), then `svc history import`; refuses if a tree hash does not
match. The store left in `<dir>/.svc` answers `svc log`, `svc blame`, `svc evolog` and the
TUI for real entities of this repository.

`agents.txt` maps each bundle to the `Agent:` trailer of its landing commit, so a git
clone (no jj) still stamps the replayed ops with who made them. Regenerate it after
adding a bundle:

```sh
for f in demo/history/[0-9]*.json; do n=$(basename $f .json); c=${n#*-*-}; \
  echo "$n $(jj --ignore-working-copy log -r "$c" --no-graph -T description | sed -n 's/^Agent: *//p' | head -1)"; \
done > demo/history/agents.txt
```
