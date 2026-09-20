# Working on svc through svc

The store is `.svc/` at the repo root (ignored by git and jj); jj stays the system of
record for landing. Every command below was run on this repo as written.

```sh
cargo build -p svc && export PATH="$PWD/target/debug:$PATH"
svc init          # once per checkout: 1853 entities, ~3 s; skips target/ .git .jj + .svcignore
svc status        # "1853 entities, 0 changes"; also absorbs any hand edit since the last verb
svc new           # open a change: "#n new change <id>"
```

## Find the thing

```sh
svc list-defs --json | jq -r '.definitions[] | select(.name=="show_def") | "\(.id) \(.file)"'
svc show-def --entity main.rs:show_def     # path-suffix:name, Type::method, or an id prefix
svc show-def --entity show_def             # ambiguity lists the candidates in both forms
svc show-def --entity <id> --json | jq -r .text > /tmp/def.rs      # the text to edit
```

## Change it

```sh
svc rename   --entity <id> --new-name <name>            # the def and every resolved use
svc edit-def --entity <id> --definition "$(cat /tmp/def.rs)" --intent refactor|fix|feature
svc add-def  --parent <impl/class id | root> --ordinal <n> --definition "..." --intent feature
svc delete   --entity <id> --intent refactor            # refused while anything references it
svc move     --entity <id> --new-parent <id>;  svc relocate --entity <id> --file <p> --ordinal <n>
```

Each verb renders the working copy and appends one op-log line; `edit-def` reports the
declared intent against the observed class (alpha / docs-only / binding-preserving /
binding-changing) and flags a mismatch. `use` lines, attributes, comments, Cargo.toml, `*.md`:
edit by hand, then `svc status` absorbs them (a changed `use` line is one removed + one added).

## Check and land

```sh
svc status                    # exactly your change, or "0 changes"
svc diff <root-a> <root-b>    # per entity: added / removed / edited: <class>; roots: svc heads
svc undo; svc op restore <n>  # undo the last op; redo it
svc replay                    # "replayed N operations: clean"
cargo test --workspace        # the rendered tree is the working copy
```

`jj describe` as usual and paste the `svc op log` lines for this change into the body.
After `jj rebase` / `jj new main` the tree moves under the store: the next `svc status`
absorbs trunk's changes as hand edits — run it before your own edit. Then leave the bundle
(`demo/history/README.md`): `svc history export --since <first op of this change> --out
demo/history/NNNN-<git sha your tree was clean at>-<change>.json`, landed with the change;
the checkout must hold no git-ignored files (svc tracks them; the base tree then differs).

## Known edges (owners in the coordination plan)

- Names resolve same file → same crate → repo; two file-level items with one name in one
  crate still alias. Check `svc show-def --json | jq .canonical` for the `#name⟨id⟩`.
- svc reads `.svcignore`, not `.gitignore`: a `result` link or demo scratch dir gets tracked.
- Two bundles may share a number; `replay.sh` orders them by the base commit, not the name.
- `svc log` renders every op it lists: ~10 s on this store; `svc status`/`diff` are sub-second.
