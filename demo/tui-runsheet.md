# svc tui — run sheet

What each key does and what appears, on this repository's own history
(`demo/dogfood.sh --tui`) or the seeded playground (`demo/play.sh --tui`). The
footer of the UI lists the same keys.

| key | in | what happens |
|---|---|---|
| `j` / `k`, `↓` / `↑` | any pane | move the selection; the right pane follows (the change's deltas, an entity's source, an op's record) |
| `Enter` | revisions / entities / op log | focus the review queue; in the queue: expand the selected edit-def to its before → after diff (again: collapse) |
| `Tab` | any | switch focus between the browse pane and the review queue |
| `e` | any | entities view: the tree of definitions (`ƒ` fn, `◇` struct/enum, `⊕` impl, `▸` mod, `·` opaque); `e` again returns to revisions |
| `/` | entities | type a filter (name, file); `Enter` keeps it, `Esc` clears it |
| `o` | any | operation log: every op newest first, with `[checkout]`; `o` again returns to revisions |
| `h` / `Esc` | any | back to the revisions view |
| `a` / `r` | review queue | allow / reject the selected pending ask from an agent (the `edit_def` permission); a verdict row is just read |
| `u` | any | `svc undo`: one step, a whole changeset when the last op belongs to one; the panes refresh |
| `p` | with `--agent` | ask the agent to continue the same task on the same session (a live model that stopped after one op) |
| `c` | with `--agent` | cancel the agent's current turn |
| `q` | any | quit; the agent session, if any, ends |

## What the panes show

- **revisions** (left): one line per change, `@` on the current one, the
  description or `(no description)`; the right pane lists that change's
  deltas against its predecessor (edited, added, relocated, rebound …).
- **entities** (left, `e`): the definition tree; the right pane shows the
  selected entity's source, then its canonical stream and blame.
- **operation log** (left, `o`): `#n verb subject [checkout]`; the right pane
  is that op's record: group, roots, observed class, the note's text.
- **review queue** (bottom): every edit-def with its declared intent and
  observed class (`✓` preserving, `✗` flagged), binding conflicts, notes
  (`[✉]` mail, verdicts, claims), and pending asks from an agent.
- **footer**: the keys; a status line when something is refused (`checkout
  busy` is retried quietly, not shown as an error).

## The demo, in keys

1. `demo/dogfood.sh --tui` — opens on the current change. `o` — the op log:
   renames, edit-defs, undos, the agents' mail; `j`×N to the run of
   `edit-def … [claude]` / `note to checkout supervisor` lines around #190.
2. `h`, then `e`, `/`, type `bundle`, `Enter` — the entities of
   `crates/svc-repo/src/bundle.rs`; `j` to `import`; the source and blame on
   the right.
3. `Tab` — the review queue; `j` to a flagged edit-def; `Enter` — its before →
   after diff. `q`.
4. Second terminal: `demo/play.sh --merge` → `svc merge a6` → the binding
   conflict; `svc resolve 0 --take accept` after the fix; `svc tui` there
   shows the merge and the resolution as two ops (`o`).
5. `demo/play.sh --agent` — line 12: the scripted agent's rename, add-def and
   edit-def stream into the queue; the edit-def ask is answered with `a`;
   `u` undoes the whole run as one changeset.

If the TUI cannot open the store ("checkout busy"), another `svc` holds the
checkout: wait a second, it retries by itself. If a live model stops after one
op, `p` continues it; the scripted agent (`demo/play.sh --agent`) is the
deterministic fallback.
