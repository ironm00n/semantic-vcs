//! A history bundle: the op log of one store, written so another checkout of the same tree
//! can replay it into its own store. Typed ops carry themselves; what they cannot carry is
//! resolved on import — entity ids by their path (file, then kind/name from the root
//! down), change ids by the order they are minted — and a hand edit (`Absorb`) carries the
//! files it changed. Every entry names the hash of the tree it left behind, so an import
//! knows at which op, if any, it stopped reproducing the recorded history.
//!
//! `svc history export` writes one; `svc history import` replays one; `demo/history/` keeps
//! the bundles of this repository's own svc-made changes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use svc_core::engine::{add_def_at, delete, edit_def, inline, move_def, relocate, render, rename};
use svc_core::{
    ChangeId, ChangeSet, ChangeSetId, EntityId, Error, Kind, NoteTo, Op, OpIx, OpLogEntry, RelPath, Result,
    Snapshot, SnapshotId,
};

use crate::history::{self, op_entity};
use crate::merge;
use crate::repo::{Provenance, Repo};

/// Where an entity sits: its file, then kind and name from the outermost parent down to
/// itself. The same tree ingested twice gives every entity a fresh id; the path is what
/// survives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityPath {
    pub file: RelPath,
    pub chain: Vec<(Kind, String)>,
}

/// A file a hand edit left: its new bytes (text when UTF-8), or removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileBody {
    Text(String),
    Bytes(Vec<u8>),
    Removed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleEntry {
    pub ix: OpIx,
    pub entry: OpLogEntry,
    /// Every entity id the op names, by path in the tree the op started from (the op's own
    /// new entity, for add-def, is absent: its id is minted by the op).
    #[serde(default)]
    pub refs: BTreeMap<EntityId, EntityPath>,
    /// For `Absorb`: the files the hand edit changed.
    #[serde(default)]
    pub files: BTreeMap<RelPath, FileBody>,
    /// blake3 over the rendered tree after this op (sorted path, bytes).
    pub after_tree: String,
    /// The checkout that made it (`None` = the default one).
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    /// blake3 of the rendered tree the first entry starts from; an import checks its own.
    pub base_tree: String,
    pub entries: Vec<BundleEntry>,
    /// The changesets the entries belong to, so a replayed op keeps its group's name.
    #[serde(default)]
    pub changesets: Vec<ChangeSet>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportReport {
    pub applied: usize,
    /// Ops whose result the engine no longer computes as recorded; the record supplied
    /// the tree instead, so the history still lands as it happened.
    pub from_record: Vec<OpIx>,
    /// First entry whose tree after import differs from the recorded hash even so, if any.
    pub diverged_at: Option<OpIx>,
}

fn tree_hash(files: &BTreeMap<RelPath, Vec<u8>>) -> String {
    let mut h = blake3::Hasher::new();
    for (path, bytes) in files {
        h.update(path.as_str().as_bytes());
        h.update(&[0]);
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    h.finalize().to_hex().to_string()
}

fn rendered(repo: &Repo, snap: &Snapshot) -> Result<BTreeMap<RelPath, Vec<u8>>> {
    Ok(render(snap, repo.store(), repo.langs(), false)?.files)
}

fn path_of(snap: &Snapshot, id: EntityId) -> Option<EntityPath> {
    let rec = snap.entities.get(&id)?;
    let mut chain = vec![(rec.kind, rec.name.clone())];
    let mut parent = rec.parent;
    while let Some(p) = parent {
        let pr = snap.entities.get(&p)?;
        chain.push((pr.kind, pr.name.clone()));
        parent = pr.parent;
    }
    chain.reverse();
    Some(EntityPath { file: rec.file.clone(), chain })
}

fn find_by_path(snap: &Snapshot, path: &EntityPath) -> Option<EntityId> {
    snap.entities
        .iter()
        .filter(|(_, r)| r.file == path.file)
        .find(|(id, _)| path_of(snap, **id).as_ref() == Some(path))
        .map(|(id, _)| *id)
}

/// Every entity id an op names besides one it mints itself.
fn named_ids(op: &Op) -> Vec<EntityId> {
    match op {
        Op::Move { id, parent, .. } => std::iter::once(*id).chain(*parent).collect(),
        Op::Extract { id, new_parent, .. } => std::iter::once(*id).chain(*new_parent).collect(),
        Op::AddDef { parent, .. } => parent.iter().copied().collect(),
        _ => op_entity(op).into_iter().collect(),
    }
}

/// The op log from `since` on, with what an import needs beside each entry.
pub fn export(repo: &Repo, since: OpIx) -> Result<Bundle> {
    export_range(repo, since, None)
}

/// [`export`] stopping before `until` (exclusive), for cutting one landing out of a longer log.
pub fn export_range(repo: &Repo, since: OpIx, until: Option<OpIx>) -> Result<Bundle> {
    let mut ops = repo.store().ops(since, false)?;
    if let Some(end) = until {
        ops.retain(|(ix, _)| *ix < end);
    }
    if ops.is_empty() {
        return Err(Error::Other(format!("no ops at or after {}", since.0)));
    }
    export_ops(repo, ops)
}

/// Whether an op is part of a changeset's story: stamped with the group, or a note
/// addressed to it (a review verdict, a claim) — which has no group of its own.
pub fn belongs(e: &OpLogEntry, group: ChangeSetId) -> bool {
    e.group == Some(group) || matches!(&e.op, Op::Note { to: NoteTo::Changeset(id), .. } if *id == group)
}

/// The ops that [`belongs`] to one changeset except those `has` — what a clone that already holds some of the
/// group's ops still lacks. Empty entries when there is nothing to send.
pub fn export_changeset(repo: &Repo, group: ChangeSetId, has: impl Fn(&OpLogEntry) -> bool) -> Result<Bundle> {
    let mut ops = repo.store().ops(OpIx(0), false)?;
    ops.retain(|(_, e)| belongs(e, group) && !has(e));
    if ops.is_empty() {
        let cs = repo.store().get_changeset(group)?;
        return Ok(Bundle { base_tree: String::new(), entries: Vec::new(), changesets: vec![cs] });
    }
    export_ops(repo, ops)
}

fn export_ops(repo: &Repo, ops: Vec<(OpIx, OpLogEntry)>) -> Result<Bundle> {
    let store = repo.store();
    let (_, first) = &ops[0];
    let base = store.get_snapshot(first.before.root)?;
    let base_tree = tree_hash(&rendered(repo, &base)?);
    let mut entries = Vec::with_capacity(ops.len());
    for (ix, e) in &ops {
        let before = store.get_snapshot(e.before.root)?;
        let after = store.get_snapshot(e.after.root)?;
        let after_files = rendered(repo, &after)?;
        let refs = named_ids(&e.op)
            .into_iter()
            .filter_map(|id| path_of(&before, id).map(|p| (id, p)))
            .collect();
        // What the op changed on disk, for every op: an absorb has nothing else, and any
        // other op falls back to it when a later engine no longer computes the same result.
        let before_files = rendered(repo, &before)?;
        let files = changed_files(&before_files, &after_files);
        entries.push(BundleEntry {
            ix: *ix,
            entry: e.clone(),
            refs,
            files,
            after_tree: tree_hash(&after_files),
            workspace: repo.redb().op_workspace(*ix)?.filter(|w| !w.is_empty()),
        });
    }
    let mut changesets = Vec::new();
    for g in entries.iter().filter_map(|b| {
        b.entry.group.or(match &b.entry.op {
            Op::Note {
                to: NoteTo::Changeset(id),
                ..
            } => Some(*id),
            _ => None,
        })
    }) {
        if !changesets.iter().any(|cs: &ChangeSet| cs.id == g)
            && let Ok(cs) = store.get_changeset(g)
        {
            changesets.push(cs);
        }
    }
    Ok(Bundle { base_tree, entries, changesets })
}

/// The files `after` has that `before` did not have with those bytes, and the ones it lost.
fn changed_files(before: &BTreeMap<RelPath, Vec<u8>>, after: &BTreeMap<RelPath, Vec<u8>>) -> BTreeMap<RelPath, FileBody> {
    let mut files = BTreeMap::new();
    for (path, bytes) in after {
        if before.get(path) != Some(bytes) {
            let body = match String::from_utf8(bytes.clone()) {
                Ok(text) => FileBody::Text(text),
                Err(_) => FileBody::Bytes(bytes.clone()),
            };
            files.insert(path.clone(), body);
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            files.insert(path.clone(), FileBody::Removed);
        }
    }
    files
}

struct Import<'a> {
    repo: &'a Repo,
    /// Recorded change id → the one minted here.
    changes: BTreeMap<ChangeId, ChangeId>,
    /// Recorded snapshot id → the one this import produced for it.
    snaps: BTreeMap<SnapshotId, SnapshotId>,
    /// Ops whose result the engine no longer reproduces and the record supplied instead.
    from_record: Vec<OpIx>,
}

impl Import<'_> {
    fn change(&self, c: ChangeId) -> ChangeId {
        *self.changes.get(&c).unwrap_or(&c)
    }

    fn entity(&self, cur: &Snapshot, b: &BundleEntry, id: EntityId) -> Result<EntityId> {
        let path = b.refs.get(&id).ok_or_else(|| {
            Error::Other(format!("op {}: no path recorded for entity {}", b.ix.0, id.short()))
        })?;
        find_by_path(cur, path).ok_or_else(|| {
            Error::Other(format!(
                "op {}: no entity at {} {} in this tree",
                b.ix.0,
                path.file,
                path.chain.iter().map(|(k, n)| format!("{k:?} {n}")).collect::<Vec<_>>().join(" / ")
            ))
        })
    }

    /// The snapshot an op leaves: what the engine computes now, when its tree is the recorded
    /// one; otherwise the recorded files, ingested over the current snapshot — the history
    /// still lands as it happened, and the op is counted as taken from the record.
    fn rewrite(
        &mut self,
        b: &BundleEntry,
        cur: &Snapshot,
        engine: impl FnOnce(&Snapshot) -> Result<Snapshot>,
    ) -> Result<Snapshot> {
        let repo = self.repo;
        let computed = match engine(cur) {
            Ok(next) => next,
            // The engine of the day refuses what it once did (a rename of a use line, say).
            // The record still says what happened: nothing, or these files.
            Err(refused) => {
                if b.files.is_empty() && tree_hash(&rendered(repo, cur)?) != b.after_tree {
                    return Err(refused);
                }
                self.from_record.push(b.ix);
                return self.from_files(b, cur);
            }
        };
        if b.files.is_empty() || tree_hash(&rendered(repo, &computed)?) == b.after_tree {
            return Ok(computed);
        }
        self.from_record.push(b.ix);
        self.from_files(b, cur)
    }

    /// The snapshot the record describes: this checkout's files with the entry's changes
    /// applied in memory (a file written to disk now would be absorbed as a hand edit first).
    /// With no recorded files it is `cur` itself: the op changed nothing.
    fn from_files(&self, b: &BundleEntry, cur: &Snapshot) -> Result<Snapshot> {
        if b.files.is_empty() {
            return Ok(cur.clone());
        }
        let repo = self.repo;
        let mut files = repo.tracked_files()?;
        for (path, body) in &b.files {
            match body {
                FileBody::Removed => {
                    files.remove(path);
                }
                FileBody::Text(t) => {
                    files.insert(path.clone(), t.as_bytes().to_vec());
                }
                FileBody::Bytes(v) => {
                    files.insert(path.clone(), v.clone());
                }
            }
        }
        repo.snapshot_files(&files, Some(cur), cur.change)
    }

    fn apply(&mut self, b: &BundleEntry) -> Result<()> {
        let repo = self.repo;
        let e = &b.entry;
        // A `checkout` between ops is not an op: the entry names the root it started from,
        // and if that is not where the last op left us, switch there first.
        if let Some(from) = self.snaps.get(&e.before.root).copied() {
            if from != repo.store().root()? {
                history::checkout(repo, from)?;
            }
        }
        // Same-snapshot rewrites: the op with its ids mapped, then the engine call the verb
        // itself makes.
        let cur = repo.current()?;
        match &e.op {
            Op::Rename { id, new } => {
                let id = self.entity(&cur, b, *id)?;
                let op = Op::Rename { id, new: new.clone() };
                let next = self.rewrite(b, &cur, |cur| rename(repo.store(), cur, id, new))?;
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::Move { id, parent, ordinal } => {
                let id = self.entity(&cur, b, *id)?;
                let parent = parent.map(|p| self.entity(&cur, b, p)).transpose()?;
                let op = Op::Move { id, parent, ordinal: *ordinal };
                let next = self.rewrite(b, &cur, |cur| move_def(repo.store(), repo.langs(), cur, id, parent, *ordinal))?;
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::Relocate { id, file, ordinal } => {
                let id = self.entity(&cur, b, *id)?;
                let op = Op::Relocate { id, file: file.clone(), ordinal: *ordinal };
                let next = self.rewrite(b, &cur, |cur| relocate(cur, repo.store(), id, file.clone(), *ordinal))?;
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::Extract { id, new_parent, ordinal } => {
                let id = self.entity(&cur, b, *id)?;
                let new_parent = new_parent.map(|p| self.entity(&cur, b, p)).transpose()?;
                let op = Op::Extract { id, new_parent, ordinal: *ordinal };
                let next = self.rewrite(b, &cur, |cur| move_def(repo.store(), repo.langs(), cur, id, new_parent, Some(*ordinal)))?;
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::Inline { id } => {
                let id = self.entity(&cur, b, *id)?;
                let next = self.rewrite(b, &cur, |cur| inline(repo.store(), repo.langs(), cur, id))?;
                repo.mutate(Op::Inline { id }, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::AddDef { id, parent, ordinal, definition, intent, file } => {
                let parent = parent.map(|p| self.entity(&cur, b, p)).transpose()?;
                let (id, ordinal, intent, file) = (*id, *ordinal, intent.clone(), file.clone());
                let op = Op::AddDef {
                    id,
                    parent,
                    ordinal,
                    definition: definition.clone(),
                    intent: intent.clone(),
                    file: file.clone(),
                };
                let next = self.rewrite(b, &cur, |cur| {
                    add_def_at(
                        repo.store(),
                        repo.langs(),
                        cur,
                        id,
                        parent,
                        file.clone(),
                        ordinal,
                        definition.as_bytes(),
                        intent.clone(),
                    )
                })?;
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::Delete { id, intent } => {
                let id = self.entity(&cur, b, *id)?;
                let op = Op::Delete { id, intent: intent.clone() };
                let next = self.rewrite(b, &cur, |cur| delete(cur, repo.store(), id))?;
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::EditDef { id, definition, intent } => {
                let id = self.entity(&cur, b, *id)?;
                let op = Op::EditDef { id, definition: definition.clone(), intent: intent.clone() };
                let next = self.rewrite(b, &cur, |cur| {
                    edit_def(repo.store(), repo.langs(), cur, id, definition.as_bytes()).map(|(next, _)| next)
                })?;
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, next))?;
            }
            Op::New { change } => {
                let out = history::new(repo)?;
                self.changes.insert(*change, out.change);
            }
            Op::Describe { msg } => {
                history::describe(repo, msg)?;
            }
            Op::Branch { name } => {
                let out = history::branch(repo, name)?;
                if let Some(recorded) = recorded_change(e) {
                    self.changes.insert(recorded, out.change);
                }
            }
            Op::Merge { other } => {
                let other = self.change(*other);
                let out = merge::merge(repo, &other.to_string())?;
                if let Some(recorded) = recorded_change(e) {
                    self.changes.insert(recorded, out.change);
                }
            }
            // Undo and restore both set a recorded view; restore the mapped ids.
            Op::Undo | Op::Restore { .. } => {
                let mut view = repo.view()?;
                if let Some(root) = self.snaps.get(&e.after.root) {
                    view.root = *root;
                }
                // A head is keyed by the change this store gave the mapped snapshot — the
                // recorded change id is the sender's, and a bundle cut after the change began
                // never named it.
                for snap in e.after.heads.values() {
                    if let Some(mapped) = self.snaps.get(snap) {
                        let change = repo.store().get_snapshot(*mapped)?.change;
                        view.heads.insert(change, *mapped);
                    }
                }
                repo.restore_view(&view, e.op.clone())?;
            }
            Op::Resolve { conflict, take } => {
                merge::resolve(repo, *conflict as usize, *take)?;
            }
            Op::Absorb => {
                write_changes(repo.root_dir(), &b.files)?;
                repo.absorb()?;
            }
            Op::Note { to, kind, text } => {
                let to = match to {
                    NoteTo::Entity(id) => NoteTo::Entity(self.entity(&cur, b, *id)?),
                    other => other.clone(),
                };
                repo.mutate(
                    Op::Note {
                        to,
                        kind: kind.clone(),
                        text: text.clone(),
                    },
                    e.observed,
                    |_, cur| Ok(cur.id()),
                )?;
            }
        }
        Ok(())
    }
}

/// The change an op minted: the head that is the op's after root.
fn recorded_change(e: &OpLogEntry) -> Option<ChangeId> {
    e.after.heads.iter().find(|(_, s)| **s == e.after.root).map(|(c, _)| *c)
}

fn write_changes(root: &std::path::Path, files: &BTreeMap<RelPath, FileBody>) -> Result<()> {
    for (path, body) in files {
        let at = root.join(path.as_str());
        match body {
            FileBody::Removed => {
                let _ = std::fs::remove_file(&at);
            }
            FileBody::Text(t) => write_file(&at, t.as_bytes())?,
            FileBody::Bytes(v) => write_file(&at, v)?,
        }
    }
    Ok(())
}

fn write_file(at: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = at.parent() {
        std::fs::create_dir_all(dir).map_err(Error::backend)?;
    }
    std::fs::write(at, bytes).map_err(Error::backend)
}

/// Replay `bundle` into `repo`, whose current tree must be the bundle's base. Stops at the
/// first entry that cannot be applied; reports the first whose result differs from the record.
pub fn import(repo: &Repo, bundle: &Bundle) -> Result<ImportReport> {
    // Notes change no tree, so a bundle of nothing else — mail, verdicts, claims — lands on
    // any tree; the base and the recorded trees only bind ops that write.
    let detached = bundle.entries.iter().all(|b| b.entry.before.root == b.entry.after.root);
    // Hand edits in this checkout are its own absorb, stamped now, not the first replayed
    // op's time and group.
    repo.absorb()?;
    let have = tree_hash(&rendered(repo, &repo.current()?)?);
    if !detached && have != bundle.base_tree {
        return Err(Error::Other(format!(
            "this tree is not the bundle's base (tree {}…, bundle starts from {}…)",
            &have[..12],
            &bundle.base_tree[..12]
        )));
    }
    for cs in &bundle.changesets {
        match repo.store().get_changeset(cs.id) {
            Ok(mut have) => {
                // The row this side holds keeps what it has (its own queue entries); the
                // sender's additions join it.
                let mut grew = false;
                for item in &cs.queue {
                    if !have.queue.contains(item) {
                        have.queue.push(item.clone());
                        grew = true;
                    }
                }
                if have.description.is_empty() && !cs.description.is_empty() {
                    have.description = cs.description.clone();
                    grew = true;
                }
                if grew {
                    repo.store().put_changeset(&have)?;
                }
            }
            Err(_) => repo.store().put_changeset(cs)?,
        }
    }
    let mut im = Import { repo, changes: BTreeMap::new(), snaps: BTreeMap::new(), from_record: Vec::new() };
    if let Some(first) = bundle.entries.first() {
        im.snaps.insert(first.entry.before.root, repo.store().root()?);
    }
    let mut diverged_at = None;
    let mut applied = 0;
    for b in &bundle.entries {
        // The op line keeps its recorded time, changeset and checkout.
        let p = Provenance { at: b.entry.at, group: b.entry.group, workspace: b.workspace.clone() };
        repo.with_provenance(p, || im.apply(b))?;
        applied += 1;
        im.snaps.insert(b.entry.after.root, repo.store().root()?);
        if !detached && diverged_at.is_none() {
            let got = tree_hash(&rendered(repo, &repo.current()?)?);
            if got != b.after_tree {
                diverged_at = Some(b.ix);
            }
        }
    }
    Ok(ImportReport { applied, from_record: im.from_record, diverged_at })
}
