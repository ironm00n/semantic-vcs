//! History verbs (SPEC §5, §8): jj-style changes over immutable snapshots.
//!
//! `evolog` walks `Snapshot.predecessors` — the *rewrite* chain of one change — never
//! `parents`. `Store::evolog` follows `predecessors.first()`, which is exact as long as an
//! amend records exactly one predecessor (it does: [`Repo::amend`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use svc_core::delta::Delta;
use svc_core::engine::status_report;
use svc_core::{
    ChangeId, ChangeSetId, EntityId, EntityRecord, Error, Intent, ObservedClass, Op, OpIx,
    OpLogEntry, RelPath, Result, Snapshot, SnapshotId, Timestamp, View,
};

use crate::repo::{Mutation, Repo};

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

fn op_out(ix: OpIx, e: &OpLogEntry) -> OpOut {
    OpOut {
        ix,
        declared: e.declared().cloned(),
        observed: e.observed,
        flagged: e.flagged(),
        at: e.at,
        group: e.group,
        root_after: e.after.root,
        op: e.op.clone(),
    }
}

fn child_of(cur: &Snapshot, cur_id: SnapshotId, change: ChangeId) -> Snapshot {
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
}

/// `svc status`: absorb hand edits into the current change (recording an `Absorb` op) and
/// report what changed. Identical content is a no-op.
pub fn status(repo: &Repo) -> Result<StatusOut> {
    let (report, snap, absorbed) = match repo.absorb()? {
        Some((prev, id)) => {
            let next = repo.store().get_snapshot(id)?;
            (status_report(&prev, &next), next, true)
        }
        None => {
            let cur = repo.current()?;
            (status_report(&cur, &cur), cur, false)
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
/// the current change's parent (DECISIONS §19) — named `name`, and made current. A root
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
/// op-log entry — and refuses to overwrite hand edits (SPEC §8).
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
        .map(|(ix, e)| op_out(*ix, e))
        .collect())
}

/// `svc log`: the op view scoped to one change (DECISIONS §20) — every op that moved that
/// change's head — newest first. Defaults to the current change.
pub fn log(repo: &Repo, change: Option<ChangeId>) -> Result<Vec<OpOut>> {
    let change = match change {
        Some(c) => c,
        None => repo.current_change()?,
    };
    Ok(repo
        .store()
        .ops(OpIx(0), true)?
        .iter()
        .filter(|(_, e)| e.before.heads.get(&change) != e.after.heads.get(&change))
        // The change's birth is not one of its events.
        .filter(|(_, e)| !matches!(e.op, Op::New { .. } | Op::Branch { .. }))
        .map(|(ix, e)| op_out(*ix, e))
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

/// `svc undo`: restore the view from before the last op. If that op belongs to a changeset,
/// the whole group is undone in one step (SPEC §5.6). The undo is itself an op, so a second
/// `undo` redoes.
pub fn undo(repo: &Repo) -> Result<MutationOut> {
    let ops = repo.store().ops(OpIx(0), true)?;
    let Some((_, last)) = ops.first() else {
        return Err(Error::Other("nothing to undo".into()));
    };
    let target = match last.group {
        Some(g) => ops
            .iter()
            .take_while(|(_, e)| e.group == Some(g))
            .last()
            .map(|(_, e)| e)
            .unwrap_or(last),
        None => last,
    };
    if target.before.heads.is_empty() {
        return Err(Error::Other("cannot undo init".into()));
    }
    let before = target.before.clone();
    let m = repo.restore_view(&before, Op::Undo)?;
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
    let m = repo.restore_view(&entry.after, Op::Undo)?;
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
}

/// `svc changeset begin <name>`: open a group that every following op is stamped with
/// (SPEC §5.6). Refuses while another is open unless `force`; a stale row is closed first.
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
    let ops = repo
        .store()
        .ops(OpIx(0), true)?
        .iter()
        .filter(|(_, e)| e.group == Some(cs.id))
        .map(|(ix, e)| op_out(*ix, e))
        .collect();
    Ok(ChangeSetOut {
        id: cs.id,
        name: cs.name,
        intent: cs.intent,
        description: cs.description,
        open,
        ops,
    })
}

/// Resolve an entity by name in the current snapshot, optionally qualified as
/// `Parent::name` (one level) to disambiguate methods of the same name.
pub fn resolve_entity(repo: &Repo, arg: &str) -> Result<EntityId> {
    let snap = repo.current()?;
    if let Some(id) = parse_entity_id(arg) {
        if snap.entities.contains_key(&id) {
            return Ok(id);
        }
    }
    let (parent, name) = match arg.rsplit_once("::") {
        Some((p, n)) => (Some(p), n),
        None => (None, arg),
    };
    let mut hits: Vec<EntityId> = snap
        .entities
        .iter()
        .filter(|(_, r)| r.name == name)
        .filter(|(_, r)| match parent {
            None => true,
            Some(p) => r
                .parent
                .and_then(|pid| snap.entities.get(&pid))
                .is_some_and(|pr| pr.name == p),
        })
        .map(|(id, _)| *id)
        .collect();
    hits.sort();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(Error::NotFound(format!("entity {arg:?}"))),
        _ => Err(Error::Other(format!(
            "{arg:?} names {} entities; qualify it as Parent::{name} or pass the id",
            hits.len()
        ))),
    }
}

pub fn parse_entity_id(s: &str) -> Option<EntityId> {
    s.parse::<uuid::Uuid>().ok().map(EntityId)
}
