//! Per-entity three-way merge (SPEC §5.3) and the `merge` / `conflicts` / `resolve` verbs.
//!
//! Sides are snapshots; the base is their lowest common ancestor over `Snapshot.parents`.
//! Entities merge by id: attributes three-way each, content by hash, add/add unified on
//! `(parent, kind, name)`. Conflicts are data in the result (jj-style), never a refusal.
//!
//! What is *not* here yet, and where it plugs in: when both sides changed one body,
//! [`merge_body`] is the seam for statement-atom diff3 and the §5.4 binding post-condition.
//! Today it records a whole-entity `Conflict::Content` and keeps side A's body.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::Serialize;
use svc_core::snapshot::AttrValue;
use svc_core::{
    Bytes, ChangeId, Chunk, Conflict, Content, EntityId, EntityRecord, Error, FileRecord,
    IdentRef, Merge, Op, RelPath, Result, Side, SigKey, Snapshot, SnapshotId, Store, Token,
};
use svc_core::engine::merge as merge_engine;

use crate::repo::Repo;

#[derive(Clone, Debug, Serialize)]
pub struct MergeOut {
    pub change: ChangeId,
    pub snapshot: SnapshotId,
    pub base: SnapshotId,
    pub conflicts: Vec<ConflictOut>,
    /// B-side ids folded into A-side ids because both added the same definition.
    pub unified: Vec<(EntityId, EntityId)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConflictOut {
    pub n: usize,
    pub entity: Option<EntityId>,
    pub name: String,
    pub conflict: Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Take {
    A,
    B,
    Base,
}

pub struct Merged {
    pub entities: BTreeMap<EntityId, EntityRecord>,
    pub files: BTreeMap<RelPath, FileRecord>,
    pub conflicts: Vec<Conflict>,
    pub unified: Vec<(EntityId, EntityId)>,
}

/// Lowest common ancestor by parent walk; `None` when the histories are unrelated.
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

fn sig(rec: &EntityRecord) -> SigKey {
    SigKey {
        parent: rec.parent,
        kind: rec.kind,
        name: rec.name.clone(),
    }
}

pub fn merge_snapshots(
    store: &dyn Store,
    base: &Snapshot,
    a: &Snapshot,
    b: &Snapshot,
) -> Result<Merged> {
    let (b_entities, unified) = unify_add_add(store, base, a, b)?;
    let mut conflicts = Vec::new();
    let mut entities = BTreeMap::new();

    let ids: BTreeSet<EntityId> = base
        .entities
        .keys()
        .chain(a.entities.keys())
        .chain(b_entities.keys())
        .copied()
        .collect();
    for id in ids {
        let o = base.entities.get(&id);
        let ra = a.entities.get(&id);
        let rb = b_entities.get(&id);
        let rec = match (o, ra, rb) {
            (_, None, None) => None,
            (None, Some(x), None) | (None, None, Some(x)) => Some(x.clone()),
            (Some(o), Some(x), None) | (Some(o), None, Some(x)) => {
                if x == o {
                    None
                } else {
                    let (deleted_by, edited_by) = if ra.is_none() {
                        (Side::A, Side::B)
                    } else {
                        (Side::B, Side::A)
                    };
                    conflicts.push(Conflict::DeleteEdit {
                        id,
                        deleted_by,
                        edited_by,
                    });
                    Some(x.clone())
                }
            }
            (o, Some(x), Some(y)) => Some(merge_record(store, id, o, x, y, &mut conflicts)?),
        };
        if let Some(rec) = rec {
            entities.insert(id, rec);
        }
    }

    assign_ordinals(base, a, &b_entities, &mut entities);

    let mut by_sig: HashMap<SigKey, Vec<EntityId>> = HashMap::new();
    for (id, rec) in &entities {
        by_sig.entry(sig(rec)).or_default().push(*id);
    }
    for (key, ids) in by_sig {
        if ids.len() > 1 {
            conflicts.push(Conflict::AddAdd {
                key,
                a: ids[0],
                b: ids[1],
            });
        }
    }

    let mut files = BTreeMap::new();
    let paths: BTreeSet<&RelPath> = base
        .files
        .keys()
        .chain(a.files.keys())
        .chain(b.files.keys())
        .collect();
    for path in paths {
        let o = base.files.get(path);
        let fa = a.files.get(path);
        let fb = b.files.get(path);
        let rec = match (o, fa, fb) {
            (_, None, None) => None,
            (None, Some(x), None) | (None, None, Some(x)) => Some(x.clone()),
            (Some(o), Some(x), None) | (Some(o), None, Some(x)) => (x != o).then(|| x.clone()),
            (o, Some(x), Some(y)) => Some(if Some(x) == o { y.clone() } else { x.clone() }),
        };
        if let Some(rec) = rec {
            files.insert(path.clone(), rec);
        }
    }
    for rec in entities.values() {
        files.entry(rec.file.clone()).or_default();
    }

    let mut all_conflicts: Vec<Conflict> = a
        .conflicts
        .iter()
        .chain(b.conflicts.iter())
        .filter(|c| !base.conflicts.contains(c))
        .cloned()
        .collect();
    all_conflicts.extend(conflicts);
    all_conflicts.dedup();

    Ok(Merged {
        entities,
        files,
        conflicts: all_conflicts,
        unified,
    })
}

fn merge_record(
    store: &dyn Store,
    id: EntityId,
    base: Option<&EntityRecord>,
    a: &EntityRecord,
    b: &EntityRecord,
    conflicts: &mut Vec<Conflict>,
) -> Result<EntityRecord> {
    let mut out = a.clone();

    let attr = |name: fn(&EntityRecord) -> AttrValue, conflicts: &mut Vec<Conflict>| {
        let (va, vb) = (name(a), name(b));
        let vo = base.map(name);
        if va == vb || Some(&vb) == vo.as_ref() {
            va
        } else if Some(&va) == vo.as_ref() {
            vb
        } else {
            conflicts.push(Conflict::Attr {
                id,
                sides: match vo {
                    Some(vo) => Merge::three_way(vo, va.clone(), vb),
                    None => Merge::from_adds_removes(vec![va.clone(), vb], vec![]).unwrap_or_else(|_| Merge::unit(va.clone())),
                },
            });
            va
        }
    };
    if let AttrValue::Name(n) = attr(|r| AttrValue::Name(r.name.clone()), conflicts) {
        out.name = n;
    }
    if let AttrValue::Parent(p) = attr(|r| AttrValue::Parent(r.parent), conflicts) {
        out.parent = p;
    }
    if let AttrValue::File(f) = attr(|r| AttrValue::File(r.file.clone()), conflicts) {
        out.file = f;
    }

    let same_a = base.is_some_and(|o| o.content == a.content && o.bytes == a.bytes);
    let same_b = base.is_some_and(|o| o.content == b.content && o.bytes == b.bytes);
    let (content, bytes) = if same_a || (a.content == b.content && a.bytes == b.bytes) {
        (b.content, b.bytes)
    } else if same_b {
        (a.content, a.bytes)
    } else if a.content == b.content {
        (a.content, a.bytes)
    } else if base.is_some_and(|o| o.content == a.content) {
        (b.content, b.bytes)
    } else if base.is_some_and(|o| o.content == b.content) {
        (a.content, a.bytes)
    } else {
        merge_body(store, id, base, a, b, conflicts)?
    };
    out.content = content;
    out.bytes = bytes;
    Ok(out)
}

/// Both sides changed one body. The seam for atom-level diff3 + the binding post-condition;
/// until those land this is a whole-entity content conflict that keeps A's body.
fn merge_body(
    _store: &dyn Store,
    id: EntityId,
    _base: Option<&EntityRecord>,
    a: &EntityRecord,
    _b: &EntityRecord,
    conflicts: &mut Vec<Conflict>,
) -> Result<(svc_core::ContentId, svc_core::BytesId)> {
    conflicts.push(Conflict::Content {
        id,
        hunks: Vec::new(),
    });
    Ok((a.content, a.bytes))
}

/// Same `(parent, kind, name)` added on both sides → one entity: keep A's id and rewrite every
/// B-side reference to it. Parents are unified before children so nested pairs line up.
fn unify_add_add(
    store: &dyn Store,
    base: &Snapshot,
    a: &Snapshot,
    b: &Snapshot,
) -> Result<(BTreeMap<EntityId, EntityRecord>, Vec<(EntityId, EntityId)>)> {
    let added_a: HashMap<SigKey, EntityId> = a
        .entities
        .iter()
        .filter(|(id, _)| !base.entities.contains_key(id))
        .map(|(id, r)| (sig(r), *id))
        .collect();
    let mut map: HashMap<EntityId, EntityId> = HashMap::new();
    let mut added_b: Vec<(EntityId, &EntityRecord)> = b
        .entities
        .iter()
        .filter(|(id, _)| !base.entities.contains_key(id) && !a.entities.contains_key(id))
        .map(|(id, r)| (*id, r))
        .collect();
    added_b.sort_by_key(|(id, r)| (depth(b, r), *id));
    for (id, rec) in added_b {
        let key = SigKey {
            parent: rec.parent.map(|p| *map.get(&p).unwrap_or(&p)),
            kind: rec.kind,
            name: rec.name.clone(),
        };
        if let Some(&aid) = added_a.get(&key) {
            map.insert(id, aid);
        }
    }
    let unified: Vec<(EntityId, EntityId)> = {
        let mut v: Vec<_> = map.iter().map(|(b, a)| (*b, *a)).collect();
        v.sort();
        v
    };
    if map.is_empty() {
        return Ok((b.entities.clone(), unified));
    }

    let mut out = BTreeMap::new();
    for (id, rec) in &b.entities {
        let mut rec = rec.clone();
        rec.parent = rec.parent.map(|p| *map.get(&p).unwrap_or(&p));
        let content = store.get_content(rec.content)?;
        if let Some(c) = remap_content(&content, &map) {
            rec.content = store.put_content(&c)?;
        }
        let bytes = store.get_bytes_blob(rec.bytes)?;
        if let Some(bts) = remap_bytes(&bytes, &map)? {
            rec.bytes = store.put_bytes_blob(&bts)?;
        }
        out.insert(*map.get(id).unwrap_or(id), rec);
    }
    Ok((out, unified))
}

fn depth(snap: &Snapshot, rec: &EntityRecord) -> usize {
    let mut d = 0;
    let mut cur = rec.parent;
    while let Some(p) = cur {
        d += 1;
        cur = snap.entities.get(&p).and_then(|r| r.parent);
    }
    d
}

fn remap_content(c: &Content, map: &HashMap<EntityId, EntityId>) -> Option<Content> {
    let mut changed = false;
    let tokens = c
        .tokens
        .iter()
        .map(|t| match t {
            Token::Ident(IdentRef::Entity(id)) if map.contains_key(id) => {
                changed = true;
                Token::Ident(IdentRef::Entity(map[id]))
            }
            Token::Child(id) if map.contains_key(id) => {
                changed = true;
                Token::Child(map[id])
            }
            t => t.clone(),
        })
        .collect();
    changed.then_some(Content { tokens })
}

fn remap_bytes(b: &Bytes, map: &HashMap<EntityId, EntityId>) -> Result<Option<Bytes>> {
    let mut changed = false;
    let chunks = b
        .chunks()
        .iter()
        .map(|c| match c {
            Chunk::Child(id) if map.contains_key(id) => {
                changed = true;
                Chunk::Child(map[id])
            }
            Chunk::Name(id) if map.contains_key(id) => {
                changed = true;
                Chunk::Name(map[id])
            }
            c => c.clone(),
        })
        .collect();
    if !changed {
        return Ok(None);
    }
    Bytes::new(b.src().to_vec(), chunks, b.local_ranges().to_vec()).map(Some)
}

/// Sibling order is layout (DECISIONS §4): start from A's order, drop what B deleted, insert
/// what B added after its B-side predecessor, then number densely. Ties go to A.
fn assign_ordinals(
    base: &Snapshot,
    a: &Snapshot,
    b: &BTreeMap<EntityId, EntityRecord>,
    out: &mut BTreeMap<EntityId, EntityRecord>,
) {
    type Group = (RelPath, Option<EntityId>);
    let groups: BTreeSet<Group> = out
        .values()
        .map(|r| (r.file.clone(), r.parent))
        .collect();
    for (file, parent) in groups {
        let seq = |ents: &BTreeMap<EntityId, EntityRecord>| -> Vec<EntityId> {
            let mut v: Vec<(u32, EntityId)> = ents
                .iter()
                .filter(|(_, r)| r.file == file && r.parent == parent)
                .map(|(id, r)| (r.ordinal, *id))
                .collect();
            v.sort();
            v.into_iter().map(|(_, id)| id).collect()
        };
        let seq_a = seq(&a.entities);
        let seq_b = seq(b);
        let in_base: BTreeSet<EntityId> = seq(&base.entities).into_iter().collect();
        let mut order: Vec<EntityId> = seq_a
            .iter()
            .copied()
            .filter(|id| out.contains_key(id))
            .collect();
        for (i, id) in seq_b.iter().enumerate() {
            if order.contains(id) || in_base.contains(id) || !out.contains_key(id) {
                continue;
            }
            let anchor = seq_b[..i]
                .iter()
                .rev()
                .find_map(|p| order.iter().position(|x| x == p));
            match anchor {
                Some(pos) => order.insert(pos + 1, *id),
                None => order.insert(0, *id),
            }
        }
        for (id, rec) in out.iter_mut() {
            if rec.file == file && rec.parent == parent {
                if let Some(pos) = order.iter().position(|x| x == id) {
                    rec.ordinal = pos as u32;
                }
            }
        }
    }
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
            let mut snap = merge_engine(repo.store(), repo.langs(), base_id, cur.id(), other_id)?;
            snap.parents = vec![cur.id(), other_id];
            snap.change = change;
            snap.predecessors = Vec::new();
            densify_ordinals(&mut snap);
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

