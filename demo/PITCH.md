# svc — pitch materials

Three pieces: the 6-minute panel script, the 90-second expo version, and the
questions we expect. Every number here has the command that produced it in
`README.md` ("Numbers"); nothing below is asserted that the terminal cannot show.

## A. Panel, 6 minutes

**0:00 — the problem, one sentence.** Review is bottlenecked by the unit of
change, and the unit of change is whatever the version-control system stores.
Git stores lines, so every review is reverse-engineering intent from a text
diff — and agents now produce more diff than anyone can read that way.

**0:30 — the one idea.** Store definitions, not lines. Every `fn`, `struct`,
class or method keeps one identity for life, a change to it is a typed
operation — rename, edit, move — and the operation carries what the author
*declared* and what the machine *observed*. Review becomes reading a journal
of what happened, with the machine's residue attached, instead of guessing
from hunks.

**1:00 — the merge git gets wrong** (slide or the block from README). One
side shadows `raw` with a normalized value; the other adds `log(&raw)` meaning
the original. The hunks do not overlap: git merges clean, the crate compiles,
the log line is wrong. `svc merge` re-resolves the merged result and says:
`binding conflict in load: raw at :73 meant the let raw at :67, now means the
let raw at :68 (shadowed)`. That is a conflict no line-based tool can see,
and it is the shape of the bug agents write.

**1:45 — live demo, 2 minutes, on the repository's own history.** All of it
was built through svc since 02:31 UTC today; the history is checked in and
replayed, not staged.

1. `demo/dogfood.sh --tui` — the review UI opens on this repository: 2,259
   entities, 213 operations in 40 bundles, each with who, when and the verdict.
   Walk the revisions (`j`/`k`, `Enter`), open the op log (`o`): renames,
   edit-defs, undos, and the mail the agents sent each other through it.
2. `q`, then in the shell (`demo/dogfood.sh --shell`): `svc rename --entity
   crates/svc-repo/src/bundle.rs:rendered --new-name rendered_files` →
   `renamed rendered → rendered_files`, its 7 mentions rewritten, one log line,
   and the sentence about what it left alone: `207 other mentions of rendered
   left unchanged (strings, comments, unrelated bindings)`. `svc undo` puts it
   back in one step; `git diff --stat` shows what git would have made of it.
3. The merge: `demo/demo-lines.sh` line 6, or the twin: `svc merge` refuses
   with the binding conflict above; `svc resolve 0 --take a` records the
   choice as an operation; `svc conflicts` is empty.
4. `svc changeset show mail` / `svc inbox` — the review and the mail between
   the agents are operations in the same log; `svc push mail ../clone` moves a
   changeset with its verdicts to another clone.
5. `demo/history/replay.sh /tmp/x` — every recorded tree reproduces, or the
   line names the op that today's engine computes differently and takes the
   record. Replay is the proof that nothing was smuggled around the journal.

**3:45 — numbers** (README "Numbers"): `syn`, 7,365 entities in 3.0 s;
`tokio`, 11,786 entities in 3.7 s, a 30-site rename across 25 files in 1.1–1.8 s
and `cargo check` still passes; 32 checkouts publishing to one store at once
without loss; a rename SIGKILLed at random points 12 times, the store never a
snapshot ahead of the log; the whole gate, `demo/run.sh`, 0 failures.

**4:30 — what is ours, honestly.** Definitions as the unit are Unison's and
sem's; ops instead of text for agents are Serena's, Rider's and CODESTRUCT's;
recording intent is Aura's; change ids and an op log are Jujutsu's; structured
merge is Mergiraf's. Ours is their intersection in a store that is not git:
the operation *is* the commit; the declared-vs-observed class is a decidable
write-time check, not a hook; the agent harness is composed without a
file-write tool; and the merge is binding-aware, not syntactic.

**5:15 — what breaks.** Lexical binding only: `x.parse()` on an unknown
receiver is a free mention and the tool says so. Rust and JavaScript. Cross-file
moves carry the text and not the imports. It is a hackathon prototype and not a
git replacement; the claim is the shape of the store, and the proof is that we
built it inside itself.

**5:45 — close.** Git made the line the unit of collaboration for humans.
Agents need a unit a machine can check. That is what svc stores.

## B. Expo, 90 seconds (pairwise)

The one thing to remember: **git stores lines; svc stores definitions, and
every change is a typed operation the machine checks against what you said
you did.**

Say: "Two agents edit one function. Git merges clean, the crate compiles, and
a log line now prints the wrong value — a reference silently moved to a
different `let`. svc stores the function as a definition, merges per
definition, re-resolves every reference, and refuses that merge with the
line that moved. Same store: a rename is one operation, not 25 files of diff;
every agent edit carries `declared refactor, observed binding-changing`; the
review and the mail between agents are operations too and travel between
clones. We built the tool's own history inside it — 213 operations,
replayed, and that replay is the demo." Then open `demo/dogfood.sh --tui`
and scroll the op log.

If there is time for one question, answer "vs git + LSP rename" from the
list below.

## C. Questions we expect

1. **Isn't this git plus an IDE rename?** An IDE rename produces a 25-file
   diff that git stores as lines; the intent is gone the moment it is
   committed. svc stores the rename, so blame, merge and review see one
   operation, and a later merge cannot re-split it.
2. **Methods and types?** Out of scope by design: binding is lexical (CST +
   name resolution), no type checker. `x.parse()` on an unknown receiver is a
   free mention and `svc rename` reports it; a compiler-backed resolver plugs
   in at one function.
3. **What about hand edits?** `svc status` absorbs them into the current
   change as an `Absorb` op with every touched entity classified; nothing is
   lost, it is just not typed.
4. **Other languages?** Rust and JavaScript today (tree-sitter grammars plus a
   per-language binder table); a language is a grammar and that table.
5. **Does it scale?** `tokio`, 555 files, 11,786 entities, init 3.7 s, status
   0.1–0.2 s, a 30-site rename under 2 s. Snapshots are stored whole and
   looked up by hash; nothing replays.
6. **How do agents use it?** Through an ACP harness (DeepSeek's dsh) whose
   schema has no `edit`/`write`; the only write tools are svc operations, and
   `edit_def` asks permission. `list_tools` is the proof.
7. **What does a conflict look like?** A binding conflict names the reference,
   the binder it meant and the binder it now means; `svc resolve <n> --take
   a|b|base` records the choice as an operation.
8. **Why not git objects underneath?** Because the store is what the review
   unit is; every 2026 semantic tool we know keeps git canonical and infers
   entities afterwards. Files are a render; `demo/git-twin` shows git's answer.
9. **What if the classifier is wrong?** It checks one decidable thing —
   did surviving references keep their binders — and says so per edit
   (`binding-preserving` / `binding-changing` / `alpha` / `docs-only`). It is a
   residue for the reviewer, not a verdict on the task.
10. **Security of the agent write path?** Enforcement by composition: the
    tool is absent from the schema, not policed. What the agent can do is what
    `svc` can do, and every op is in the log with the checkout that made it.
11. **What's next?** A type-aware resolver, imports on cross-file moves, and
    a real clone/sync protocol (today a changeset moves between clones as a
    bundle; the shape is there, the transport is a directory).
12. **Who built it — AI tools?** Owen (ironmoon) directed; the code was
    written by Claude Code, Codex, Cursor, Devin, Warp and Muse agents working in
    parallel jj workspaces, coordinating first on a shared board and, from
    06:33 UTC, through `svc mail` inside the tool. The history in
    `demo/history/` is the record of that.
