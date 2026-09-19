//! The `merge` / `conflicts` / `resolve` verbs over `svc_core::engine::merge` (SPEC §5.3–5.4).
//!
//! Sides are snapshots; the base is their lowest common ancestor over `Snapshot.parents`.
//! The engine does the per-entity three-way, the atom-level body merge and the binding
//! post-condition; this module supplies the LCA, the change wiring, dense ordinals, the
//! conflict numbering `resolve` uses, and `--take` resolution. Conflicts are data in the
//! result (jj-style), never a refusal.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use svc_core::engine::merge as merge_engine;
use svc_core::snapshot::AttrValue;
use svc_core::{
    ChangeId, Conflict, EntityId, EntityRecord, Error, Op, RelPath, Result, Snapshot, SnapshotId,
    Store,
};

use crate::repo::Repo;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeOut {
    pub change: ChangeId,
    pub snapshot: SnapshotId,
    pub base: SnapshotId,
    pub conflicts: Vec<ConflictOut>,
    /// B-side ids folded into A-side ids because both added the same definition.
    pub unified: Vec<(EntityId, EntityId)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConflictOut {
    pub n: usize,
    pub entity: Option<EntityId>,
    pub name: String,
    pub conflict: Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Take {
    A,
    B,
    Base,
}

pub fn lca(store: &dyn Store, a: SnapshotId, b: SnapshotId) -> Result<Option<SnapshotId>> {
    let mut seen_a = BTreeSet::new();
    let mut queue = VecDeque::from([a]);
    while let Some(id) = queue.pop_front() {
        if seen_a.insert(id) {
            queue.extend(store.get_snapshot(id)?.parents);
        }
    }
    let mut seen_b = BTreeSet::new();
    let mut queue = VecDeque::from([b]);
    while let Some(id) = queue.pop_front() {
        if seen_a.contains(&id) {
            return Ok(Some(id));
        }
        if seen_b.insert(id) {
            queue.extend(store.get_snapshot(id)?.parents);
        }
    }
    Ok(None)
}

fn densify_ordinals(snap: &mut Snapshot) {
    let groups: BTreeSet<(RelPath, Option<EntityId>)> = snap
        .entities
        .values()
        .map(|r| (r.file.clone(), r.parent))
        .collect();
    for (file, parent) in groups {
        let mut ids: Vec<(u32, EntityId)> = snap
            .entities
            .iter()
            .filter(|(_, r)| r.file == file && r.parent == parent)
            .map(|(id, r)| (r.ordinal, *id))
            .collect();
        ids.sort();
        for (i, (_, id)) in ids.iter().enumerate() {
            if let Some(rec) = snap.entities.get_mut(id) {
                rec.ordinal = i as u32;
            }
        }
    }
}

fn unified_from(b: &Snapshot, merged: &Snapshot) -> Vec<(EntityId, EntityId)> {
    b.entities
        .iter()
        .filter(|(id, _)| !merged.entities.contains_key(id))
        .filter_map(|(bid, rec)| {
            merged
                .entities
                .iter()
                .find(|(_, r)| r.parent == rec.parent && r.kind == rec.kind && r.name == rec.name)
                .map(|(aid, _)| (*bid, *aid))
        })
        .collect()
}

pub fn conflict_entity(c: &Conflict) -> Option<EntityId> {
    match c {
        Conflict::Attr { id, .. }
        | Conflict::Content { id, .. }
        | Conflict::DeleteEdit { id, .. }
        | Conflict::Binding { id, .. } => Some(*id),
        Conflict::AddAdd { a, .. } => Some(*a),
    }
}

fn conflicts_out(snap: &Snapshot) -> Vec<ConflictOut> {
    snap.conflicts
        .iter()
        .enumerate()
        .map(|(n, c)| {
            let entity = conflict_entity(c);
            ConflictOut {
                n,
                entity,
                name: entity
                    .and_then(|id| snap.entities.get(&id))
                    .map(|r| r.name.clone())
                    .unwrap_or_default(),
                conflict: c.clone(),
            }
        })
        .collect()
}

/// `svc merge <change|branch>`: a new change with parents `[current, other]` holding the
/// merged snapshot, conflicts included. Renders and logs like every mutation.
pub fn merge(repo: &Repo, other: &str) -> Result<MergeOut> {
    let other_change = repo.resolve_change(other)?;
    let store = repo.store();
    let other_id = store.head(other_change)?;
    let cur_id = store.root()?;
    let base_id = lca(store, cur_id, other_id)?
        .ok_or_else(|| Error::Other("no common ancestor".into()))?;
    if base_id == other_id {
        return Err(Error::Other(format!(
            "{} is already an ancestor of the current change",
            other_change.short()
        )));
    }
    let change = ChangeId::new();
    let mut report = None;
    let m = repo.mutate(
        Op::Merge {
            other: other_change,
        },
        None,
        |repo, cur| {
            let b = repo.store().get_snapshot(other_id)?;
            let snap = merged_snapshot(repo.store(), repo.langs(), base_id, cur.id(), other_id, change)?;
            let unified = unified_from(&b, &snap);
            report = Some((conflicts_out(&snap), unified));
            repo.commit_snapshot(&snap)
        },
    )?;
    let (conflicts, unified) = report.unwrap_or_default();
    Ok(MergeOut {
        change,
        snapshot: m.snapshot,
        base: base_id,
        conflicts,
        unified,
    })
}

/// The merge result as a new change: `engine::merge` plus the wiring (`parents`, `change`,
/// dense ordinals). Shared by the verb and the O5 replay so both produce the same snapshot.
pub fn merged_snapshot(
    store: &dyn Store,
    langs: &svc_core::Langs,
    base: SnapshotId,
    a: SnapshotId,
    b: SnapshotId,
    change: ChangeId,
) -> Result<Snapshot> {
    let mut snap = merge_engine(store, langs, base, a, b)?;
    snap.parents = vec![a, b];
    snap.change = change;
    snap.predecessors = Vec::new();
    densify_ordinals(&mut snap);
    Ok(snap)
}

/// `svc conflicts`: the current snapshot's conflicts, numbered for `resolve`.
pub fn conflicts(repo: &Repo) -> Result<Vec<ConflictOut>> {
    Ok(conflicts_out(&repo.current()?))
}

/// `svc resolve <n> --take a|b|base`: choose one side of an attribute, content, delete/edit or
/// add/add conflict. Binding conflicts have no side to take (SPEC §5.4) and are rejected here.
pub fn resolve(repo: &Repo, n: usize, take: Take) -> Result<MergeOut> {
    let cur = repo.current()?;
    let conflict = cur
        .conflicts
        .get(n)
        .cloned()
        .ok_or_else(|| Error::NotFound(format!("conflict {n}")))?;
    let [a_id, b_id] = cur.parents.as_slice() else {
        return Err(Error::Other("current snapshot is not a merge".into()));
    };
    let (a_id, b_id) = (*a_id, *b_id);
    let store = repo.store();
    let side_a = store.get_snapshot(a_id)?;
    let side_b = store.get_snapshot(b_id)?;
    let base_id = lca(store, a_id, b_id)?
        .ok_or_else(|| Error::Other("no common ancestor".into()))?;
    let base = store.get_snapshot(base_id)?;
    let pick = |id: EntityId| -> Option<EntityRecord> {
        match take {
            Take::A => side_a.entities.get(&id).cloned(),
            Take::B => side_b.entities.get(&id).cloned(),
            Take::Base => base.entities.get(&id).cloned(),
        }
    };

    let mut next = cur.clone();
    match &conflict {
        Conflict::Binding { .. } => {
            return Err(Error::Other(
                "a binding conflict has no side to take; fix the code or `resolve --accept`".into(),
            ));
        }
        Conflict::Attr { id, sides } => {
            let value = match take {
                Take::A => sides.adds().next().cloned(),
                Take::B => sides.adds().nth(1).cloned(),
                Take::Base => sides.removes().next().cloned(),
            }
            .ok_or_else(|| Error::Other("that side has no value".into()))?;
            let rec = next.entities.get_mut(id).ok_or(Error::NoSuchEntity(*id))?;
            match value {
                AttrValue::Name(n) => rec.name = n,
                AttrValue::Parent(p) => rec.parent = p,
                AttrValue::File(f) => rec.file = f,
                AttrValue::Ordinal(o) => rec.ordinal = o,
            }
        }
        Conflict::Content { id, .. } => {
            let chosen = pick(*id).ok_or(Error::NoSuchEntity(*id))?;
            let rec = next.entities.get_mut(id).ok_or(Error::NoSuchEntity(*id))?;
            rec.content = chosen.content;
            rec.bytes = chosen.bytes;
        }
        Conflict::DeleteEdit { id, .. } => match pick(*id) {
            Some(rec) => {
                next.entities.insert(*id, rec);
            }
            None => {
                next.entities.remove(id);
            }
        },
        Conflict::AddAdd { a, b, .. } => {
            let drop = match take {
                Take::A => *b,
                Take::B => *a,
                Take::Base => {
                    return Err(Error::Other("add/add has no base side".into()));
                }
            };
            next.entities.remove(&drop);
        }
    }
    next.conflicts.remove(n);
    let m = repo.mutate(
        Op::Describe {
            msg: cur.message.clone(),
        },
        None,
        |repo, cur| repo.amend(cur, next),
    )?;
    let snap = repo.current()?;
    Ok(MergeOut {
        change: snap.change,
        snapshot: m.snapshot,
        base: base_id,
        conflicts: conflicts_out(&snap),
        unified: Vec::new(),
    })
}

