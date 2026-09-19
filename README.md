# svc — compiler-grade version control

Agent-generated code is cheap. Understanding it is not.

`svc` is a semantic version-control system. Definitions have stable identities,
a rename is recorded as a rename, and a merge that would silently retarget a
reference is a conflict — even when git would take both hunks and the crate
would still compile.

It is not a git frontend. The working tree is a *render* of the store. An
agent on the included DeepSeek Harness overlay can read the tree normally,
but its only write tools are `svc` operations: no file editor, no shell.

## What is stored

Git answers "what does the tree look like" by reading a commit's tree
object. `svc` does the same: a **snapshot** is stored whole and looked up
by id. Checkout, show, and render never replay operations to reconstruct
it.

What's *inside* that snapshot is not git blobs. A language file is a list
of **entities** (`fn`, `struct`, `impl`, JS functions and methods, …), each
with one `EntityId` for life, plus leftover bytes after the last item.
Render walks that list in ordinal order. Non-language files (`Cargo.toml`,
this README) are those leftover bytes with no entities.

The snapshot lives under `SnapshotId = blake3(snapshot)`. Unchanged
*content* is shared by hash; the entity index is copied in full (no
git-style subtree sharing yet).

| git | svc |
|---|---|
| commit + tree | `SnapshotId` — one immutable map of every entity |
| blob | `BytesId` (exact source with holes) and `ContentId` (α-normal form) |
| branch name | a name for a `ChangeId` |
| `git checkout <commit>` | `get_snapshot(id)` |
| reflog / `jj evolog` | `predecessors` on a change |
| `git log -p` | the **op log** — a journal of named operations whose `before`/`after` *point at* snapshots |

A `ChangeId` is not a snapshot. It is a stable name for a logical change
(jj, not git). Amending moves `head(change)`; old states of that change are
on `predecessors`. Merge ancestry is `parents`. Neither walk answers "what
does the tree look like."

The op log is how you *review* what happened. A rename is an `Op::Rename`,
not a 26-file text diff inferred after the fact. `svc replay` folds that
journal onto an empty store only as a check that nothing was smuggled
around it.

Each entity is stored twice:

- **Content** — locals become slots, callees become entity ids, trivia is
  gone. Renaming `parse` changes one string in one record; call sites are
  holes filled at render time.
- **Bytes** — the original spelling, comments, and layout, with `Name(id)`
  and `Child(id)` holes so the file round-trips.

## The merge git gets wrong

One side shadows `raw` with a normalized value. The other adds `log(&raw)`
intending the original binding. The hunks do not overlap. Git merges clean
and the crate compiles. The log call now prints the normalized string.

`svc` merges per entity, then re-resolves the result. If a surviving
reference denotes a different binder than it did on the side that wrote it,
that is a `Conflict::Binding` on the snapshot:

```text
$ svc merge a6
merged into change 8230 (snapshot 58f0): 1 conflict(s)
    [0] binding conflict in load: `raw` at src/main.rs:73 meant the `let raw`
        at src/main.rs:67, now means the `let raw` at src/main.rs:68 (shadowed)
```

Reproduce the git side without `svc`:

```sh
demo/git-twin/build.sh
```

## Try it

```sh
nix develop
cargo build -p svc
demo/play.sh          # scratch copy of the demo crate, svc on PATH
# demo/play.sh --tui
# demo/play.sh --agent
```

Or by hand:

```sh
cd demo/config
../../target/debug/svc init
../../target/debug/svc list-defs
../../target/debug/svc show parse
../../target/debug/svc rename --entity parse --new-name parse_config
../../target/debug/svc log
```

Every verb prints a sentence; add `--json` for the machine form.
`svc rename` says how many mentions it could not track (method calls on
untyped receivers, strings, comments) instead of pretending.

The acceptance gate is one script:

```sh
demo/run.sh target/debug/svc
```

That runs the scripted demo (rename, the binding-conflict merge, the JS
twin, the agent changeset, evolog/undo, the in-TUI replay, the forge, and
self-hosting `svc` on this repo's own crates), then a multi-checkout stress
test and 32 checkouts publishing at once. `SVC_SKIP_SELF_HOST=1` skips the
self-host line.

Self-hosting alone:

```sh
cargo build -p svc && demo/self-host.sh
```

## Agent overlay

Read, glob, and grep stay. `edit` and `write` are removed from the schema;
shell, web, and subagents are disabled. Writes go through `rename`,
`add_def`, `edit_def`, and the rest. `edit_def` must be a complete item and
asks permission. `list_tools` is the proof of the tool set.

```sh
SVC_BIN="$PWD/target/debug/svc" OPENROUTER_API_KEY="…" \
  npx -y @deepseek-ai/dsh@0.1.5-rc.2 \
    --profile acp --patch harness/overlay.yml
```

`svc tui` is the review UI (entity tree, canonical stream, op history,
review queue). `svc tui --agent "<task>"` runs that task inside it; the
run is one changeset, so `svc undo` reverts it in one step. Without a
model key, `demo/play.sh --agent` hosts a scripted ACP agent against the
real binary. Any ACP-on-stdio agent works the same way.

Live A/B (stock dsh vs overlay) is `demo/ab.sh`. Without a credential it
checks identical starting trees and exits 0 with SKIP.

## Forge

`svc forge export` writes `.svc/forge.json` from the store (never contends
with a writer). `crates/svc-forge` serves it:

```sh
svc forge export
cargo run -p svc-forge -- --catalog .svc/forge.json   # http://127.0.0.1:7742
```

Heads and the current root carry their entities. Ancestor snapshots in the
export are listed by id only — the store still has the full snapshots;
the catalog file does not reconstruct them by replaying operations.

## Scale

`svc init` on `syn` (97 files) ingested 7,365 entities in 14 s; on `tokio`
(555 files) 11,786 entities in 19 s. `svc status` on either is 0.06–0.15 s.
Renaming `tokio`'s `asyncify` (30 call sites across 26 files) is one log
line, 0.26 s, and `cargo check --features full` still passes. Git shows the
same change as 26 files, 55 insertions, 55 deletions.

## Limits

Hackathon prototype. Not a git replacement.

- **Lexical binding only.** `x.parse()` on an unknown receiver is a free
  mention. Method, field, and trait-item resolution needs types; the store
  shape does not. `svc rename` reports the misses.
- **Rust and JavaScript.** JS import/export across files is not modeled.
- **Macros are opaque token trees.** Identifiers inside `foo!(x)` are not
  locals.
- **Non-language files** (`Cargo.toml`, lockfiles, this README) are stored
  as opaque byte records so a checkout still builds. They are not entities.
- **Classifier** checks surviving-reference capture, not "did the agent do
  the task."
- **Concurrency:** named checkouts share one store (redb multi-writer: any
  number of `svc` processes, write transactions serialized on the file). A
  publish that finds its change's head moved is refused with nothing written
  (`concurrent update`), not merged. A checkout is one working copy: one
  `svc` at a time (`checkout busy` after a bounded wait). Each checkout undoes
  only its own ops and refuses to mutate while behind its change's head.
- **Live models.** With `deepseek-chat`, a run sometimes stops after the
  first op; the TUI's `p` continues the same session. The scripted agent
  is the deterministic gate.

## Built with

HackMIT 2026. Codex, Claude Code, Muse, Cursor, and DeepSeek Harness.
Rust, tree-sitter (Rust and JavaScript), redb, postcard, BLAKE3, similar,
clap, ratatui, agent-client-protocol, axum, Node.js, `@deepseek-ai/dsh@0.1.5-rc.2`.

Prior art: MolhadoRef, Mergiraf, jj, Serena, semedit, CODESTRUCT, IDE
refactorings. The experiment is their intersection in a store that is not
git, with agents that cannot write files.

Licensed under the GNU Affero General Public License v3.0 or later
(`LICENSE`).
