# svc — compiler-grade version control

![Two definitions, one reference, and a merge that refuses to redirect the binding](svc-cover.svg)

`svc` is a version-control system whose unit is the definition, not the line.
Every `fn`, `struct`, `impl`, class or method keeps one identity for life; a
rename is recorded as a rename; a merge that would silently make a reference
point at a different binder is a conflict, even when git merges clean and the
crate still compiles. The working tree is a render of the store, and an agent
on the included overlay can only write through `svc` operations. This
repository's own history since 02:31 UTC was made with it: 213 operations in
40 bundles, replayed by the first command below.

## Try it

```sh
nix develop            # or: a Rust toolchain plus jq and node
cargo build -p svc
demo/dogfood.sh --tui  # this repository's own svc-made history, replayed, in the review UI
```

The third command rebuilds `demo/history/*.json` into a fresh store from
git's trees (a minute or two), then opens `svc tui` on it. `--shell` instead
of `--tui` opens a shell in that checkout.

## What you will see

```text
$ svc op log
#212 absorbed hand edits  [cursor]
#199 edit-def open_with⟨8f4511f6⟩ (declared feature, observed binding-preserving) ✓  [claude]
#194 note to checkout supervisor: "landed qkyvkunv 6fddc064 (Touch::Rebound + mail-sync fixes …"  [claude]
#175 undo  [claude]
#110 renamed hex4 → short_hex  [codex]

$ svc blame --entity crates/svc-repo/src/bundle.rs:import
#172 change 91e5f023 edited (binding-preserving) — edit-def import⟨86564d5f⟩ (declared fix, unchecked)
#160 change 91e5f023 relocated crates/svc-repo/src/bundle.rs#25 → crates/svc-repo/src/bundle.rs#28
```

- **History in operations.** `svc op log` is a journal of named operations —
  rename, edit-def, add-def, undo, note — each with who made it, when, and the
  classifier's verdict, not a diff inferred afterwards. `svc blame` on an
  entity lists the operations that touched it. (In the TUI: `j`/`k` and `Enter`
  through revisions, `e` entities, `o` the op log, `u` undo, `q` quit.)
- **A rename follows.** `svc rename --entity parse --new-name parse_config`
  rewrites every resolved mention and says what it could not resolve:
  `1 method call .parse(…) left unchanged: receiver types are not resolved`.
  On `tokio`, renaming `asyncify` (30 call sites, 25 files) is one log line.
- **The merge git gets wrong is a conflict.** See the block below; `svc
  conflicts` lists it, `svc resolve <n> --take a|b|base` records the choice
  as an operation, and `svc replay` re-derives the result.
- **Review lives in the repository.** A changeset groups operations; `svc
  review <changeset> --approve|--request-changes|--note`, `svc mail` and `svc
  claim` are operations too. `svc push <changeset> <dir>` carries them to
  another clone, which then prints the same `svc changeset show`. The
  coordination between the agents that built this repository ran through it
  from 06:33 UTC (the notes are in the replayed history).
- **Replay is the proof.** `demo/history/replay.sh <dir>` refuses any bundle
  whose recorded trees do not reproduce; when today's engine computes an old
  operation differently, the recorded files supply the tree and the line says so
  (`ops 32,36 from the record`).

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

Reproduce the git side without `svc`: `demo/git-twin/build.sh`.

## How it works

**Entities with two encodings.** A language file is a list of entities, each
with one `EntityId` for life. Each is stored twice: *content* — locals become
slots, callees become entity ids, trivia is gone, so renaming `parse` changes
one string in one record and call sites are holes filled at render time — and
*bytes* — the original spelling, comments and layout, with `Name(id)` and
`Child(id)` holes so the file round-trips byte for byte. Files with no
language (`Cargo.toml`, this README) are opaque byte records.

**Snapshots plus a journal.** A snapshot is one immutable map of every entity,
stored whole under `blake3(snapshot)` and looked up by id — checkout, show and
render never replay anything. A `ChangeId` is a stable name for a logical change
(jj, not git): amending moves its head, old states stay on `predecessors`,
merge ancestry is `parents`. The op log is a separate journal whose entries
point at snapshots; `svc undo` walks it back, `svc op restore <n>` forward.

**Binding-aware merge.** Merge is per entity; the result is re-resolved and
every surviving reference is checked against the binder it had on the side
that wrote it. A changed binder is a binding conflict; a changed body with
the same bindings is not. The classifier gives every `edit-def` a verdict the
same way (binding-preserving, binding-changing, alpha, docs-only), which is
what the review queue is made of.

**Review, mail and claims are operations.** A changeset is a group of
operations; a verdict, a note to another checkout or a claim on an entity is
an `Op::Note` in the same log. `svc push`/`svc pull` move a changeset between
clones as a history bundle — only what the other side lacks, matched by time
and content, not by index — so the receiving clone shows the same queue. No
server holds it; `svc-forge` and the TUI only render the store.

## Numbers

Debug build on a shared VM under load, the numbers a judge running `cargo
build -p svc` gets:

| what | number | command |
|---|---|---|
| this repository ingested | 2,259 entities in 208 files, 2.6 s | `svc init` at the repo root (`demo/selfhost-full.sh` does it in a copy) |
| its own history replayed | 40 bundles, 213 operations, every tree as recorded | `demo/history/replay.sh <dir>` |
| the rendered tree still builds | `cargo test --workspace` from an empty checkout: 0 failures | `demo/selfhost-full.sh` |
| `syn` (97 files) | 7,365 entities in 3.0 s; `status` 0.1–0.2 s | `svc init` in a `syn` checkout |
| `tokio` (555 files) | 11,786 entities in 3.7 s; rename of `asyncify` (30 sites, 25 files) 1.1–1.8 s, `cargo check --features full` passes; git shows 25 files, 55/55 lines | `svc rename --entity asyncify --new-name …` |
| one store, many checkouts | 32 checkouts publishing at once: 30 renames, indices contiguous, no loss | `tests/concurrency/workspace_stress.sh 32` |
| kill it mid-write | 12 `SIGKILL`s at random points of a rename: never a snapshot ahead of the log | `demo/crash.sh` |
| the whole gate | `cargo test --workspace` 357/0; `demo/run.sh` 138 checks, 0 failures | `demo/run.sh target/debug/svc` |

`demo/run.sh` runs the scripted demo lines, the forge, self-hosting on this
repository, the multi-checkout stress, the crash test, two checkouts of this
repository merging one file (conflict, resolution, replay, type-check), the
changeset sync (`demo/sync.sh`), the history replay and O9, the compiler
oracle over the binder table.

## Agent overlay

Read, glob and grep stay; `edit` and `write` are removed from the schema;
shell, web and subagents are disabled. Writes go through `rename`, `add_def`,
`edit_def` and the rest; `edit_def` must be a complete item and asks
permission. `list_tools` is the proof of the tool set.

```sh
SVC_BIN="$PWD/target/debug/svc" OPENROUTER_API_KEY="…" \
  npx -y @deepseek-ai/dsh@0.1.5-rc.2 --profile acp --patch harness/overlay.yml
```

`svc tui --agent "<task>"` runs that task inside the review UI; the run is one
changeset, so `svc undo` reverts it in one step. Without a model key,
`demo/play.sh --agent` hosts a scripted ACP agent against the real binary.

## Verbs

`status` (absorbs hand edits into the current change), `log`, `op log`, `show`,
`blame`, `evolog`, `heads`, `rename`, `move`, `relocate`, `extract`, `inline`,
`add-def`, `edit-def`, `delete`, `undo`, `op restore`, `new`, `describe`,
`branch`, `merge`, `conflicts`, `resolve`, `replay`, `changeset begin|end|
reopen|show|list`, `review`, `mail`, `inbox`, `claim`, `release`, `push`,
`pull`, `workspace add|list`, `history export|import`, `forge export`, `tui`.
The verbs people read at the expo print a sentence; the rest print JSON;
`--json` on any verb is the machine form.

## Limits

Hackathon prototype. Not a git replacement.

- **Lexical binding only.** `x.parse()` on an unknown receiver is a free
  mention; method, field and trait-item resolution needs types. `svc rename`
  reports the misses.
- **Rust and JavaScript.** Named ESM `import { x }` follows a unique `export`
  across files (ambiguous names stay put); default-import aliases stay local,
  and CJS `require()` / `module.exports` pairs are not modeled.
- **Macros.** Arguments of a macro call resolve like any other code (`vec![x]`
  uses the local `x`, as rustc says — O9 enforces it); `macro_rules!` bodies
  are not analysed.
- **Cross-file moves** carry the text and nothing else: imports are not
  rewritten; `svc undo` restores the tree.
- **Regular files only.** Symlinks, FIFOs, sockets and devices are not
  tracked; a file name that is not UTF-8 is refused by name. `.svcignore`
  (bare names, no globs) is read; `.gitignore` is not.
- **Concurrency.** Named checkouts share one store (redb multi-writer). A
  publish that finds its change's head moved is refused with nothing written
  (`concurrent update`); a checkout is one working copy at a time (`checkout
  busy` after a bounded wait); each checkout undoes only its own operations.
- **Sync** moves a changeset between clones on the same tree (a fresh clone,
  or one that took the previous push); notes alone land on any tree.
- **Live models.** With `deepseek-chat`, a run sometimes stops after the
  first operation; the TUI's `p` continues the same session. The scripted
  agent is the deterministic gate.

## Built with

svc itself: this repository's changes since 02:31 UTC were made through
`svc` verbs and are the 40 bundles in `demo/history/` (213 operations), the
agents' coordination through `svc mail` from 06:33 UTC. HackMIT 2026. Codex,
Claude Code, Muse, Cursor, Devin, Warp and DeepSeek Harness. Rust,
tree-sitter (Rust and JavaScript), redb, postcard, BLAKE3, similar, clap,
ratatui, agent-client-protocol, axum, Node.js,
`@deepseek-ai/dsh@0.1.5-rc.2`.

Prior art: MolhadoRef, Mergiraf, jj, Serena, semedit, CODESTRUCT, IDE
refactorings. The experiment is their intersection in a store that is not
git, with agents that cannot write files.

Licensed under the GNU Affero General Public License v3.0 or later
(`LICENSE`).
