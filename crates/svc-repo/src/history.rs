//! History verbs: jj-style changes over immutable snapshots.
//!
//! `evolog` walks `Snapshot.predecessors` — the *rewrite* chain of one change — never
//! `parents`. `Store::evolog` follows `predecessors.first()`, which is exact as long as an
//! amend records exactly one predecessor (it does: [`Repo::amend`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use svc_core::delta::Delta;
use svc_core::engine::status_report;
use svc_core::{
    ChangeId, ChangeSetId, EntityId, EntityRecord, Error, Intent, NoteKind, NoteTo, ObservedClass,
    Op, OpIx, OpLogEntry, RelPath, Result, Snapshot, SnapshotId, Timestamp, View,
};

use crate::repo::{Mutation, Repo};
use crate::workspace::DEFAULT_WORKSPACE;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeOut {
    pub change: ChangeId,
    pub short: String,
    pub snapshot: SnapshotId,
    pub message: String,
    pub branches: Vec<String>,
    pub current: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpOut {
    pub ix: OpIx,
    pub op: Op,
    pub declared: Option<Intent>,
    pub observed: Option<ObservedClass>,
    pub flagged: bool,
    pub at: Timestamp,
    pub group: Option<ChangeSetId>,
    pub root_after: SnapshotId,
    /// The entity's name *before* the op (renames need it; the current snapshot has the new one).
    pub subject: Option<String>,
    /// The checkout that made it (absent = the default one).
    #[serde(default)]
    pub workspace: Option<String>,
}

/// How one op or rewrite touched one entity. `Edited.observed` is the op log's verdict
/// for that op, absent when nothing classified it (a hand edit, or a class not yet landed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Touch {
    Added,
    Removed,
    Renamed { from: String, to: String },
    Moved { from: Option<EntityId>, to: Option<EntityId> },
    Relocated { from: (RelPath, u32), to: (RelPath, u32) },
    Edited { observed: Option<ObservedClass> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityTouch {
    pub entity: EntityId,
    pub name: String,
    pub touch: Touch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvologEntry {
    pub snapshot: SnapshotId,
    pub message: String,
    pub entities: usize,
    /// Versus the previous (older) entry; empty for the first.
    pub deltas: Vec<EntityTouch>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlameEntry {
    pub ix: OpIx,
    pub change: ChangeId,
    pub op: Op,
    pub touch: Touch,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MutationOut {
    pub ix: OpIx,
    pub change: ChangeId,
    pub snapshot: SnapshotId,
    pub closed_stale_changeset: Option<ChangeSetId>,
}

impl MutationOut {
    fn of(repo: &Repo, m: Mutation) -> Result<Self> {
        Ok(Self {
            ix: m.ix,
            change: repo.store().get_snapshot(m.entry.after.root)?.change,
            snapshot: m.snapshot,
            closed_stale_changeset: m.closed_stale_changeset,
        })
    }
}

fn op_out(repo: &Repo, ix: OpIx, e: &OpLogEntry) -> OpOut {
    let store = repo.store();
    // Name as of just before the op (so a rename shows its old name); for ops that create
    // the entity (add-def, extract) fall back to the name just after. Stored on the entry
    // so the line stays readable after the entity is later deleted or the op undone.
    let subject = op_entity(&e.op).and_then(|id| {
        let name_in = |root| {
            store
                .get_snapshot(root)
                .ok()
                .and_then(|s| s.entities.get(&id).map(|r| r.name.clone()))
        };
        name_in(e.before.root).or_else(|| name_in(e.after.root))
    });
    OpOut {
        ix,
        subject,
        declared: e.declared().cloned(),
        observed: e.observed,
        flagged: e.flagged(),
        at: e.at,
        group: e.group,
        root_after: e.after.root,
        op: e.op.clone(),
        workspace: repo.redb().op_workspace(ix).ok().flatten().filter(|w| !w.is_empty()),
    }
}

pub(crate) fn child_of(cur: &Snapshot, cur_id: SnapshotId, change: ChangeId) -> Snapshot {
    Snapshot {
        parents: vec![cur_id],
        predecessors: Vec::new(),
        change,
        entities: cur.entities.clone(),
        files: cur.files.clone(),
        conflicts: cur.conflicts.clone(),
        message: String::new(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusOut {
    pub summary: String,
    pub entities: usize,
    pub layout: usize,
    pub semantic: usize,
    pub clean: bool,
    pub change: ChangeId,
    pub snapshot: SnapshotId,
    /// Semantic deltas versus the snapshot before this status (layout-only ordinal shifts suppressed).
    pub deltas: Vec<Delta>,
    pub absorbed: bool,
    /// Unresolved merge conflicts on the current snapshot (`svc conflicts` lists them).
    pub conflicts: usize,
    /// Other checkouts' live claims on entities in this tree (`svc claim`).
    #[serde(default)]
    pub claims: Vec<ClaimOut>,
    /// Unread mail/review notes addressed here or `@all`.
    #[serde(default)]
    pub inbox: usize,
}

/// `svc status`: absorb hand edits into the current change (recording an `Absorb` op) and
/// report what changed. Identical content is a no-op.
pub fn status(repo: &Repo) -> Result<StatusOut> {
    let (report, snap, absorbed) = match repo.absorb()? {
        Some((prev, id)) => {
            let next = repo.store().get_snapshot(id)?;
            (status_report(repo.store(), &prev, &next)?, next, true)
        }
        None => {
            let cur = repo.current()?;
            (status_report(repo.store(), &cur, &cur)?, cur, false)
        }
    };
    Ok(StatusOut {
        summary: report.summary(),
        entities: report.entities,
        layout: report.layout,
        semantic: report.semantic,
        clean: report.deltas.is_empty(),
        change: snap.change,
        snapshot: snap.id(),
        deltas: report.deltas,
        absorbed,
        conflicts: snap.conflicts.len(),
        claims: active_claims(repo)?,
        inbox: inbox(repo)?.unread.len(),
    })
}

/// `svc new`: start the next change on top of the current one.
pub fn new(repo: &Repo) -> Result<MutationOut> {
    let change = ChangeId::new();
    let m = repo.mutate(Op::New { change }, None, |repo, cur| {
        repo.commit_snapshot(&child_of(cur, cur.id(), change))
    })?;
    MutationOut::of(repo, m)
}

/// `svc describe`: set the current change's message (an amend).
pub fn describe(repo: &Repo, msg: &str) -> Result<MutationOut> {
    let m = repo.mutate(Op::Describe { msg: msg.into() }, None, |repo, cur| {
        let mut next = cur.clone();
        next.message = msg.into();
        repo.amend(cur, next)
    })?;
    MutationOut::of(repo, m)
}

/// `svc branch <name>`: a new change that is a *sibling* of the current one — its parent is
/// the current change's parent — named `name`, and made current. A root
/// change has no parent, so its sibling would be empty; branch from the root itself instead.
pub fn branch(repo: &Repo, name: &str) -> Result<MutationOut> {
    if repo.store().branch(name)?.is_some() {
        return Err(Error::Other(format!("branch {name:?} already exists")));
    }
    let change = ChangeId::new();
    let m = repo.mutate(Op::Branch { name: name.into() }, None, |repo, cur| {
        let (base, base_id) = match cur.parents.first() {
            Some(p) => (repo.store().get_snapshot(*p)?, *p),
            None => (cur.clone(), cur.id()),
        };
        let id = repo.commit_snapshot(&child_of(&base, base_id, change))?;
        repo.store().set_branch(name, change)?;
        Ok(id)
    })?;
    MutationOut::of(repo, m)
}

/// `svc edit <change|branch>`: make another change's head the working copy.
pub fn edit(repo: &Repo, target: &str) -> Result<View> {
    let change = repo.resolve_change(target)?;
    let head = repo.store().head(change)?;
    checkout(repo, head)
}

/// `svc checkout <snapshot>`: view a snapshot. Moves `root` only — no change, no head, no
/// op-log entry — and refuses to overwrite hand edits.
pub fn checkout(repo: &Repo, snapshot: SnapshotId) -> Result<View> {
    if !repo.working_copy_clean()? {
        return Err(Error::Other(
            "working copy has edits not yet snapshotted; run `svc status` first".into(),
        ));
    }
    let snap = repo.store().get_snapshot(snapshot)?;
    repo.store().set_render_pending(true)?;
    repo.store().set_root(snapshot)?;
    repo.render_to_disk(&snap)?;
    repo.store().set_render_pending(false)?;
    repo.view()
}

/// `svc heads`: every change and where it points, current first.
pub fn heads(repo: &Repo) -> Result<Vec<ChangeOut>> {
    let store = repo.store();
    let current = repo.current_change()?;
    let mut names: BTreeMap<ChangeId, Vec<String>> = BTreeMap::new();
    for (name, id) in store.branches()? {
        names.entry(id).or_default().push(name);
    }
    let mut out = Vec::new();
    for (change, snapshot) in store.heads()? {
        out.push(ChangeOut {
            change,
            short: change.short(),
            snapshot,
            message: store.get_snapshot(snapshot)?.message,
            branches: names.remove(&change).unwrap_or_default(),
            current: change == current,
        });
    }
    out.sort_by_key(|c| !c.current);
    Ok(out)
}

/// `svc op log`: the unscoped journal, newest first.
pub fn op_log(repo: &Repo) -> Result<Vec<OpOut>> {
    Ok(repo
        .store()
        .ops(OpIx(0), true)?
        .iter()
        .map(|(ix, e)| op_out(repo, *ix, e))
        .collect())
}

/// `svc log`: the op view scoped to one change — every op that moved that
/// change's head or ran on it (a no-op `edit-def` still happened and still queues for
/// review) — newest first. Defaults to the current change.
pub fn log(repo: &Repo, change: Option<ChangeId>) -> Result<Vec<OpOut>> {
    let change = match change {
        Some(c) => c,
        None => repo.current_change()?,
    };
    let store = repo.store();
    let on_change = |e: &OpLogEntry| {
        e.before.heads.get(&change) != e.after.heads.get(&change)
            || store.get_snapshot(e.after.root).is_ok_and(|s| s.change == change)
    };
    Ok(store
        .ops(OpIx(0), true)?
        .iter()
        .filter(|(_, e)| on_change(e))
        // The change's birth is not one of its events.
        .filter(|(_, e)| !matches!(e.op, Op::New { .. } | Op::Branch { .. }))
        .map(|(ix, e)| op_out(repo, *ix, e))
        .collect())
}

/// `svc log <entity>`: blame at entity granularity — which ops touched it and how.
pub fn blame(repo: &Repo, entity: EntityId) -> Result<Vec<BlameEntry>> {
    let store = repo.store();
    let mut out = Vec::new();
    for (ix, e) in store.ops(OpIx(0), true)? {
        if e.before.root == e.after.root {
            continue;
        }
        let before = store.get_snapshot(e.before.root)?;
        let after = store.get_snapshot(e.after.root)?;
        if let Some(touch) = touch(
            before.entities.get(&entity),
            after.entities.get(&entity),
            e.observed,
        ) {
            out.push(BlameEntry {
                ix,
                change: after.change,
                op: e.op.clone(),
                touch,
                at: e.at,
            });
        }
    }
    Ok(out)
}

/// `svc evolog <change>`: the change's snapshots newest first, each with what it changed
/// versus the one it rewrote.
pub fn evolog(repo: &Repo, change: ChangeId) -> Result<Vec<EvologEntry>> {
    let store = repo.store();
    let snaps = store.evolog(change)?;
    let ops = store.ops(OpIx(0), false)?;
    let mut out = Vec::with_capacity(snaps.len());
    for (i, snap) in snaps.iter().enumerate() {
        let id = snap.id();
        let observed = ops
            .iter()
            .find(|(_, e)| e.after.heads.get(&change) == Some(&id))
            .and_then(|(_, e)| e.observed);
        let deltas = match snaps.get(i + 1) {
            Some(prev) => touches(prev, snap, observed),
            None => Vec::new(),
        };
        out.push(EvologEntry {
            snapshot: id,
            message: snap.message.clone(),
            entities: snap.entities.len(),
            deltas,
        });
    }
    Ok(out)
}

/// `svc undo`: restore the view from before this checkout's newest op that no earlier undo
/// already reverted; an op that belongs to a changeset takes the whole group with it.
/// Redo is `svc op restore <n>`.
pub fn undo(repo: &Repo) -> Result<MutationOut> {
    // Each checkout undoes its own ops: another checkout's entry carries
    // *its* root in `before`, and restoring that here would silently switch checkouts.
    // Unattributed entries from before op attribution count as the default checkout's.
    let ops = repo.redb().own_ops(OpIx(0), true)?;
    // Repeated `undo` keeps walking back (as `jj undo` does since 0.29) instead of undoing
    // the previous undo: skip every op an earlier undo already reverted. An undo of a
    // changeset reverts the whole group, so it cancels that many ops.
    let entries: Vec<&OpLogEntry> = ops.iter().map(|(_, e)| e).collect(); // newest first
    let mut i = 0;
    let mut pending = 0usize;
    let first = loop {
        let Some(e) = entries.get(i) else {
            return Err(Error::Other("nothing to undo".into()));
        };
        if matches!(e.op, Op::Undo) {
            pending += 1;
            i += 1;
        } else if pending > 0 {
            // reverted by an earlier undo — together with the rest of its changeset
            pending -= 1;
            let g = e.group;
            i += 1;
            while g.is_some() && entries.get(i).is_some_and(|n| n.group == g) {
                i += 1;
            }
        } else {
            break i;
        }
    };
    let last = entries[first];
    let target = match last.group {
        Some(g) => entries[first..]
            .iter()
            .take_while(|e| e.group == Some(g))
            .last()
            .copied()
            .unwrap_or(last),
        None => last,
    };
    if target.before.heads.is_empty() {
        return Err(Error::Other("cannot undo init".into()));
    }
    // Only what the undone ops moved goes back: this checkout's root and the heads whose
    // value differs across the range. Another checkout's head that moved since is its
    // business, not something an undo here may rewind.
    let mut view = repo.view()?;
    view.root = target.before.root;
    for (change, snap) in &target.before.heads {
        if last.after.heads.get(change) != Some(snap) {
            view.heads.insert(*change, *snap);
        }
    }
    let m = repo.restore_view(&view, Op::Undo)?;
    MutationOut::of(repo, m)
}

/// `svc op restore <n>`: return to the view as it stood right after op `n`.
pub fn op_restore(repo: &Repo, ix: OpIx) -> Result<MutationOut> {
    let entry = repo
        .store()
        .ops(ix, false)?
        .into_iter()
        .find(|(i, _)| *i == ix)
        .map(|(_, e)| e)
        .ok_or_else(|| Error::NotFound(format!("op {}", ix.0)))?;
    let m = repo.restore_view(&entry.after, Op::Restore { at: ix.0 })?;
    MutationOut::of(repo, m)
}

/// Attribute-level difference of one entity between two snapshots. Content that moved
/// without any attribute moving is `Edited`.
pub fn touch(
    prev: Option<&EntityRecord>,
    next: Option<&EntityRecord>,
    observed: Option<ObservedClass>,
) -> Option<Touch> {
    match (prev, next) {
        (None, None) => None,
        (None, Some(_)) => Some(Touch::Added),
        (Some(_), None) => Some(Touch::Removed),
        (Some(a), Some(b)) => {
            if a.name != b.name {
                Some(Touch::Renamed {
                    from: a.name.clone(),
                    to: b.name.clone(),
                })
            } else if a.parent != b.parent {
                Some(Touch::Moved {
                    from: a.parent,
                    to: b.parent,
                })
            } else if a.file != b.file || a.ordinal != b.ordinal {
                Some(Touch::Relocated {
                    from: (a.file.clone(), a.ordinal),
                    to: (b.file.clone(), b.ordinal),
                })
            } else if a.content != b.content || a.bytes != b.bytes {
                Some(Touch::Edited { observed })
            } else {
                None
            }
        }
    }
}

/// Every entity `touch`ed between two snapshots, in id order.
pub fn touches(prev: &Snapshot, next: &Snapshot, observed: Option<ObservedClass>) -> Vec<EntityTouch> {
    let ids = prev.entities.keys().chain(next.entities.keys());
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for id in ids {
        if !seen.insert(*id) {
            continue;
        }
        let a = prev.entities.get(id);
        let b = next.entities.get(id);
        if let Some(t) = touch(a, b, observed) {
            out.push(EntityTouch {
                entity: *id,
                name: b.or(a).map(|r| r.name.clone()).unwrap_or_default(),
                touch: t,
            });
        }
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeSetOut {
    pub id: ChangeSetId,
    pub name: String,
    pub intent: Intent,
    pub description: String,
    pub open: bool,
    pub ops: Vec<OpOut>,
    /// Review/mail notes hung on this changeset (the other half of a PR).
    #[serde(default)]
    pub reviews: Vec<OpOut>,
}

/// `svc changeset begin <name>`: open a group that every following op is stamped with.
/// Refuses while another is open unless `force`; a stale row is closed first.
pub fn changeset_begin(
    repo: &Repo,
    name: &str,
    intent: Intent,
    pid: Option<u32>,
    force: bool,
) -> Result<ChangeSetOut> {
    let store = repo.store();
    if let (Some(open), _) = repo.open_group()? {
        if !force {
            return Err(Error::Other(format!(
                "changeset {} is already open; `svc changeset end` it or pass --force",
                open.short()
            )));
        }
    }
    let cs = svc_core::ChangeSet {
        id: ChangeSetId::new(),
        name: name.into(),
        intent,
        queue: Vec::new(),
        description: String::new(),
    };
    store.put_changeset(&cs)?;
    store.set_open_changeset(Some(svc_core::OpenChangeSet {
        id: cs.id,
        pid,
        opened_at: crate::repo::now(),
    }))?;
    changeset_out(repo, cs, true)
}

/// `svc changeset end`: close the open group. Returns what was closed, if anything.
pub fn changeset_end(repo: &Repo) -> Result<Option<ChangeSetId>> {
    let open = repo.store().open_changeset()?.map(|r| r.id);
    repo.store().set_open_changeset(None)?;
    Ok(open)
}

/// `svc changeset status`: the open group and its ops so far, or `None`.
pub fn changeset_status(repo: &Repo) -> Result<Option<ChangeSetOut>> {
    match repo.open_group()? {
        (Some(id), _) => changeset_out(repo, repo.store().get_changeset(id)?, true).map(Some),
        _ => Ok(None),
    }
}

/// `svc changesets`: every group, with its ops.
pub fn changesets(repo: &Repo) -> Result<Vec<ChangeSetOut>> {
    let open = repo.open_group()?.0;
    repo.store()
        .changesets()?
        .into_iter()
        .map(|cs| {
            let is_open = open == Some(cs.id);
            changeset_out(repo, cs, is_open)
        })
        .collect()
}

fn changeset_out(repo: &Repo, cs: svc_core::ChangeSet, open: bool) -> Result<ChangeSetOut> {
    let mut ops = Vec::new();
    let mut reviews = Vec::new();
    for (ix, e) in repo.store().ops(OpIx(0), true)? {
        let about = matches!(
            &e.op,
            Op::Note {
                to: NoteTo::Changeset(id),
                kind,
                ..
            } if *id == cs.id && !matches!(kind, NoteKind::Claim | NoteKind::Release)
        );
        if e.group == Some(cs.id) || about {
            let out = op_out(repo, ix, &e);
            if about {
                reviews.push(out.clone());
            }
            ops.push(out);
        }
    }
    Ok(ChangeSetOut {
        id: cs.id,
        name: cs.name,
        intent: cs.intent,
        description: cs.description,
        open,
        ops,
        reviews,
    })
}

/// Resolve an entity by name in the current snapshot, optionally qualified as
/// `Parent::name` (one level) to disambiguate methods of the same name.
/// `svc … --entity X`: an id, a name, `Parent::name` (the parent's name, or for an impl
/// block the type it is for), or `path/suffix.rs:name`. Ambiguity lists the candidates.
pub fn resolve_entity(repo: &Repo, arg: &str) -> Result<EntityId> {
    resolve_entity_in(&repo.current()?, arg)
}

/// [`resolve_entity`] against a chosen snapshot (a historical `--at`). An id or an id
/// prefix wins outright; names are matched exactly.
pub fn resolve_entity_in(snap: &Snapshot, arg: &str) -> Result<EntityId> {
    if let Some(id) = parse_entity_id(arg) {
        if snap.entities.contains_key(&id) {
            return Ok(id);
        }
    }
    let spec_hits: Vec<EntityId> = snap.entities.keys().copied().filter(|id| id.matches_spec(arg)).collect();
    let (file, rest) = match arg.rsplit_once(':').filter(|(f, _)| f.contains('/') || f.contains('.')) {
        Some((f, n)) if !f.ends_with(':') => (Some(f), n),
        _ => (None, arg),
    };
    let (parent, name) = match rest.rsplit_once("::") {
        Some((p, n)) => (Some(p), n),
        None => (None, rest),
    };
    let mut hits: Vec<EntityId> = snap
        .entities
        .iter()
        .filter(|(_, r)| r.name == name)
        .filter(|(_, r)| file.is_none_or(|f| r.file.as_str().ends_with(f)))
        .filter(|(_, r)| match parent {
            None => true,
            Some(p) => r
                .parent
                .and_then(|pid| snap.entities.get(&pid))
                .is_some_and(|pr| pr.name == p || impl_target(&pr.name) == Some(p)),
        })
        .map(|(id, _)| *id)
        .collect();
    hits.sort();
    match hits.len() {
        1 => Ok(hits[0]),
        0 if spec_hits.len() == 1 => Ok(spec_hits[0]),
        0 => Err(Error::NotFound(format!("entity {arg:?}"))),
        _ => {
            let listed: Vec<String> = hits
                .iter()
                .map(|id| {
                    let r = &snap.entities[id];
                    let parent = r
                        .parent
                        .and_then(|p| snap.entities.get(&p))
                        .map(|p| format!("{}::", impl_target(&p.name).unwrap_or(&p.name)))
                        .unwrap_or_default();
                    format!("{}:{parent}{} ⟨{}⟩", r.file, r.name, id.short())
                })
                .collect();
            Err(Error::Other(format!(
                "{arg:?} names {} entities — {}; qualify it as Parent::{name} or file.rs:{name}, or pass the id",
                hits.len(),
                listed.join(", ")
            )))
        }
    }
}

/// The type an impl block's synthesised name is for: `impl<Foo>` and `impl<Tr for Foo>` → `Foo`.
fn impl_target(name: &str) -> Option<&str> {
    let inner = name.strip_prefix("impl<")?.strip_suffix('>')?;
    Some(inner.rsplit_once(" for ").map_or(inner, |(_, t)| t))
}

pub fn parse_entity_id(s: &str) -> Option<EntityId> {
    s.parse::<uuid::Uuid>().ok().map(EntityId)
}

/// The entity an op is about, when it is about one.
pub fn op_entity(op: &Op) -> Option<EntityId> {
    match op {
        Op::Rename { id, .. }
        | Op::Move { id, .. }
        | Op::Relocate { id, .. }
        | Op::Extract { id, .. }
        | Op::Inline { id }
        | Op::AddDef { id, .. }
        | Op::Delete { id, .. }
        | Op::EditDef { id, .. } => Some(*id),
        Op::Note {
            to: NoteTo::Entity(id),
            ..
        } => Some(*id),
        _ => None,
    }
}

/// Whole-word mentions of `word` still in the rendered working copy after a rename. Every
/// reference svc resolved has already changed; what remains is what it could not resolve —
/// method calls on typed receivers (`x.word(…)`, which need types), and strings, comments
/// or unrelated bindings — and did not touch. The honest numbers to print next to
/// "renamed" (demo line 13), not to hide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mentions {
    pub method_calls: usize,
    pub other: usize,
}

pub fn untracked_mentions(repo: &Repo, word: &str) -> Result<Mentions> {
    let snap = repo.current()?;
    let is_ident = |c: char| c == '_' || c.is_ascii_alphanumeric();
    let mut m = Mentions::default();
    for path in snap.files.keys() {
        let Ok(text) = std::fs::read_to_string(repo.root_dir().join(path.as_str())) else {
            continue;
        };
        let mut from = 0;
        while let Some(i) = text[from..].find(word) {
            let start = from + i;
            let end = start + word.len();
            from = end;
            let head = &text[..start];
            let tail = &text[end..];
            if head.chars().next_back().is_some_and(is_ident) || tail.chars().next().is_some_and(is_ident) {
                continue;
            }
            let method_call = head.trim_end().ends_with('.') && tail.trim_start().starts_with('(');
            if method_call {
                m.method_calls += 1;
            } else {
                m.other += 1;
            }
        }
    }
    Ok(m)
}

/// The checkout name stamped on notes (`default` when this is the `.svc/` tree).
pub fn checkout_name(repo: &Repo) -> String {
    repo.workspace()
        .unwrap_or(DEFAULT_WORKSPACE)
        .to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimOut {
    pub entity: EntityId,
    pub name: String,
    pub by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InboxOut {
    pub unread: Vec<OpOut>,
    pub cursor: u64,
}

/// Append a coordination op. Snapshot is unchanged; the log is the mailbox.
pub fn note(repo: &Repo, to: NoteTo, kind: NoteKind, text: impl Into<String>) -> Result<MutationOut> {
    let text = text.into();
    if matches!(kind, NoteKind::Note) && text.trim().is_empty() {
        return Err(Error::Other("note text is empty".into()));
    }
    let m = repo.mutate(
        Op::Note { to, kind, text },
        None,
        |_, cur| Ok(cur.id()),
    )?;
    MutationOut::of(repo, m)
}

pub fn resolve_changeset(repo: &Repo, spec: &str) -> Result<svc_core::ChangeSet> {
    let all = repo.store().changesets()?;
    let named: Vec<_> = all.iter().filter(|c| c.name == spec).cloned().collect();
    match named.len() {
        1 => return Ok(named.into_iter().next().unwrap()),
        n if n > 1 => {
            return Err(Error::Other(format!(
                "changeset {spec:?} names {n} groups — pass the id"
            )));
        }
        _ => {}
    }
    match svc_core::ids::resolve_spec(all.iter().map(|c| c.id), |id| {
        id.matches_spec(spec) || id.to_string() == spec
    }) {
        Ok(id) => all
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Error::NotFound(format!("changeset {spec:?}"))),
        Err(hits) if hits.is_empty() => Err(Error::NotFound(format!("changeset {spec:?}"))),
        Err(_) => Err(Error::Other(format!(
            "changeset {spec:?} is ambiguous — pass more of the id"
        ))),
    }
}

pub fn changeset_show(repo: &Repo, spec: &str) -> Result<ChangeSetOut> {
    let cs = resolve_changeset(repo, spec)?;
    let open = repo.open_group()?.0 == Some(cs.id);
    changeset_out(repo, cs, open)
}

/// `svc review <changeset> --approve|--request-changes|--note`.
pub fn review(
    repo: &Repo,
    spec: &str,
    kind: NoteKind,
    text: &str,
) -> Result<MutationOut> {
    if !matches!(
        kind,
        NoteKind::Approve | NoteKind::RequestChanges | NoteKind::Note
    ) {
        return Err(Error::Other("review kind must be approve, request-changes, or note".into()));
    }
    let cs = resolve_changeset(repo, spec)?;
    note(repo, NoteTo::Changeset(cs.id), kind, text)
}

/// `svc mail <checkout|@all> "<text>" [--about entity|changeset]`.
pub fn mail(
    repo: &Repo,
    to: &str,
    text: &str,
    about: Option<&str>,
) -> Result<MutationOut> {
    let dest = resolve_mail_to(repo, to, about)?;
    note(repo, dest, NoteKind::Note, text)
}

fn resolve_mail_to(repo: &Repo, to: &str, about: Option<&str>) -> Result<NoteTo> {
    if let Some(about) = about {
        if let Ok(id) = resolve_entity(repo, about) {
            return Ok(NoteTo::Entity(id));
        }
        if let Ok(cs) = resolve_changeset(repo, about) {
            return Ok(NoteTo::Changeset(cs.id));
        }
        return Err(Error::NotFound(format!(
            "--about {about:?} is not an entity or a changeset"
        )));
    }
    if to == "@all" {
        Ok(NoteTo::All)
    } else {
        Ok(NoteTo::Checkout(to.into()))
    }
}

fn note_is_mail(op: &Op) -> bool {
    match op {
        Op::Note { kind, .. } => !matches!(kind, NoteKind::Claim | NoteKind::Release),
        _ => false,
    }
}

fn note_addressed_to(op: &Op, me: &str) -> bool {
    match op {
        Op::Note { to, .. } => match to {
            NoteTo::All => true,
            NoteTo::Checkout(name) => name == me,
            NoteTo::Changeset(_) | NoteTo::Entity(_) => true,
        },
        _ => false,
    }
}

pub fn inbox(repo: &Repo) -> Result<InboxOut> {
    let me = checkout_name(repo);
    let cursor = repo.redb().inbox_read_ix()?;
    let unread = repo
        .store()
        .ops(OpIx(0), false)?
        .into_iter()
        .filter(|(ix, e)| {
            ix.0 > cursor && note_is_mail(&e.op) && note_addressed_to(&e.op, &me)
        })
        .map(|(ix, e)| op_out(repo, ix, &e))
        .collect();
    Ok(InboxOut { unread, cursor })
}

/// `svc mail --read <n>`: mark notes through op `n` as read.
pub fn mail_read(repo: &Repo, ix: u64) -> Result<InboxOut> {
    repo.redb().set_inbox_read_ix(ix)?;
    inbox(repo)
}

fn claim_ids(repo: &Repo, spec: &str) -> Result<Vec<EntityId>> {
    if let Ok(id) = resolve_entity(repo, spec) {
        return Ok(vec![id]);
    }
    let snap = repo.current()?;
    let ids: Vec<EntityId> = snap
        .entities
        .iter()
        .filter(|(_, r)| r.file.as_str() == spec || r.file.as_str().ends_with(&format!("/{spec}")))
        .map(|(id, _)| *id)
        .collect();
    if ids.is_empty() {
        Err(Error::NotFound(format!(
            "{spec:?} is not an entity or a tracked file"
        )))
    } else {
        Ok(ids)
    }
}

pub fn claim(repo: &Repo, specs: &[String]) -> Result<Vec<MutationOut>> {
    if specs.is_empty() {
        return Err(Error::Other("claim: pass an entity or a file".into()));
    }
    let me = checkout_name(repo);
    let mut out = Vec::new();
    for spec in specs {
        for id in claim_ids(repo, spec)? {
            out.push(note(
                repo,
                NoteTo::Entity(id),
                NoteKind::Claim,
                me.clone(),
            )?);
        }
    }
    Ok(out)
}

pub fn release(repo: &Repo, specs: &[String]) -> Result<Vec<MutationOut>> {
    let me = checkout_name(repo);
    let ids = if specs.is_empty() {
        active_claims(repo)?
            .into_iter()
            .filter(|c| c.by == me)
            .map(|c| c.entity)
            .collect()
    } else {
        let mut ids = Vec::new();
        for spec in specs {
            ids.extend(claim_ids(repo, spec)?);
        }
        ids
    };
    let mut out = Vec::new();
    for id in ids {
        out.push(note(
            repo,
            NoteTo::Entity(id),
            NoteKind::Release,
            me.clone(),
        )?);
    }
    Ok(out)
}

/// Latest Claim/Release per entity; only live Claims remain.
pub fn active_claims(repo: &Repo) -> Result<Vec<ClaimOut>> {
    let mut last: BTreeMap<EntityId, (NoteKind, String)> = BTreeMap::new();
    for (ix, e) in repo.store().ops(OpIx(0), false)? {
        if let Op::Note {
            to: NoteTo::Entity(id),
            kind,
            ..
        } = &e.op
        {
            if matches!(kind, NoteKind::Claim | NoteKind::Release) {
                let by = repo
                    .redb()
                    .op_workspace(ix)
                    .ok()
                    .flatten()
                    .filter(|w| !w.is_empty())
                    .unwrap_or_else(|| DEFAULT_WORKSPACE.to_string());
                last.insert(*id, (kind.clone(), by));
            }
        }
    }
    let snap = repo.current()?;
    let mut out: Vec<ClaimOut> = last
        .into_iter()
        .filter_map(|(id, (kind, by))| {
            if kind != NoteKind::Claim {
                return None;
            }
            let name = snap
                .entities
                .get(&id)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| id.short());
            Some(ClaimOut {
                entity: id,
                name,
                by,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.by.cmp(&b.by)));
    Ok(out)
}

/// Notes hung on an entity (show-def / the other half of a review thread).
pub fn notes_about_entity(repo: &Repo, id: EntityId) -> Result<Vec<OpOut>> {
    Ok(repo
        .store()
        .ops(OpIx(0), true)?
        .into_iter()
        .filter(|(_, e)| {
            matches!(
                &e.op,
                Op::Note {
                    to: NoteTo::Entity(eid),
                    ..
                } if *eid == id
            )
        })
        .map(|(ix, e)| op_out(repo, ix, &e))
        .collect())
}
