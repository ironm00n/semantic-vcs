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
    ChangeId, EntityId, Error, Kind, Op, OpIx, OpLogEntry, RelPath, Result, Snapshot, SnapshotId,
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
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportReport {
    pub applied: usize,
    /// First entry whose tree after import differs from the recorded hash, if any.
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
    let store = repo.store();
    let ops = store.ops(since, false)?;
    let Some((_, first)) = ops.first() else {
        return Err(Error::Other(format!("no ops at or after {}", since.0)));
    };
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
        let mut files = BTreeMap::new();
        if matches!(e.op, Op::Absorb) {
            let before_files = rendered(repo, &before)?;
            for (path, bytes) in &after_files {
                if before_files.get(path) != Some(bytes) {
                    let body = match String::from_utf8(bytes.clone()) {
                        Ok(text) => FileBody::Text(text),
                        Err(_) => FileBody::Bytes(bytes.clone()),
                    };
                    files.insert(path.clone(), body);
                }
            }
            for path in before_files.keys() {
                if !after_files.contains_key(path) {
                    files.insert(path.clone(), FileBody::Removed);
                }
            }
        }
        entries.push(BundleEntry {
            ix: *ix,
            entry: e.clone(),
            refs,
            files,
            after_tree: tree_hash(&after_files),
            workspace: repo.redb().op_workspace(*ix)?.filter(|w| !w.is_empty()),
        });
    }
    Ok(Bundle { base_tree, entries })
}

struct Import<'a> {
    repo: &'a Repo,
    /// Recorded change id → the one minted here.
    changes: BTreeMap<ChangeId, ChangeId>,
    /// Recorded snapshot id → the one this import produced for it.
    snaps: BTreeMap<SnapshotId, SnapshotId>,
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
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, rename(cur, id, new)?))?;
            }
            Op::Move { id, parent, ordinal } => {
                let id = self.entity(&cur, b, *id)?;
                let parent = parent.map(|p| self.entity(&cur, b, p)).transpose()?;
                let op = Op::Move { id, parent, ordinal: *ordinal };
                repo.mutate(op, e.observed, |repo, cur| {
                    repo.amend(cur, move_def(repo.store(), repo.langs(), cur, id, parent, *ordinal)?)
                })?;
            }
            Op::Relocate { id, file, ordinal } => {
                let id = self.entity(&cur, b, *id)?;
                let op = Op::Relocate { id, file: file.clone(), ordinal: *ordinal };
                repo.mutate(op, e.observed, |repo, cur| {
                    repo.amend(cur, relocate(cur, repo.store(), id, file.clone(), *ordinal)?)
                })?;
            }
            Op::Extract { id, new_parent, ordinal } => {
                let id = self.entity(&cur, b, *id)?;
                let new_parent = new_parent.map(|p| self.entity(&cur, b, p)).transpose()?;
                let op = Op::Extract { id, new_parent, ordinal: *ordinal };
                repo.mutate(op, e.observed, |repo, cur| {
                    repo.amend(cur, move_def(repo.store(), repo.langs(), cur, id, new_parent, Some(*ordinal))?)
                })?;
            }
            Op::Inline { id } => {
                let id = self.entity(&cur, b, *id)?;
                repo.mutate(Op::Inline { id }, e.observed, |repo, cur| {
                    repo.amend(cur, inline(repo.store(), repo.langs(), cur, id)?)
                })?;
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
                repo.mutate(op, e.observed, |repo, cur| {
                    let next = add_def_at(
                        repo.store(),
                        repo.langs(),
                        cur,
                        id,
                        parent,
                        file.clone(),
                        ordinal,
                        definition.as_bytes(),
                        intent.clone(),
                    )?;
                    repo.amend(cur, next)
                })?;
            }
            Op::Delete { id, intent } => {
                let id = self.entity(&cur, b, *id)?;
                let op = Op::Delete { id, intent: intent.clone() };
                repo.mutate(op, e.observed, |repo, cur| repo.amend(cur, delete(cur, repo.store(), id)?))?;
            }
            Op::EditDef { id, definition, intent } => {
                let id = self.entity(&cur, b, *id)?;
                let op = Op::EditDef { id, definition: definition.clone(), intent: intent.clone() };
                repo.mutate(op, e.observed, |repo, cur| {
                    let (next, _) = edit_def(repo.store(), repo.langs(), cur, id, definition.as_bytes())?;
                    repo.amend(cur, next)
                })?;
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
            // An undo and an `op restore` are both logged as Undo; what they did is the view
            // they left, so restore that view with its ids mapped.
            Op::Undo => {
                let mut view = repo.view()?;
                if let Some(root) = self.snaps.get(&e.after.root) {
                    view.root = *root;
                }
                for (change, snap) in &e.after.heads {
                    if let Some(mapped) = self.snaps.get(snap) {
                        view.heads.insert(self.change(*change), *mapped);
                    }
                }
                repo.restore_view(&view, Op::Undo)?;
            }
            Op::Resolve { conflict, take } => {
                merge::resolve(repo, *conflict as usize, *take)?;
            }
            Op::Absorb => {
                let root = repo.root_dir();
                for (path, body) in &b.files {
                    let at = root.join(path.as_str());
                    match body {
                        FileBody::Removed => {
                            let _ = std::fs::remove_file(&at);
                        }
                        FileBody::Text(t) => write_file(&at, t.as_bytes())?,
                        FileBody::Bytes(v) => write_file(&at, v)?,
                    }
                }
                repo.absorb()?;
            }
        }
        Ok(())
    }
}

/// The change an op minted: the head that is the op's after root.
fn recorded_change(e: &OpLogEntry) -> Option<ChangeId> {
    e.after.heads.iter().find(|(_, s)| **s == e.after.root).map(|(c, _)| *c)
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
    let have = tree_hash(&rendered(repo, &repo.current()?)?);
    if have != bundle.base_tree {
        return Err(Error::Other(format!(
            "this tree is not the bundle's base (tree {}…, bundle starts from {}…)",
            &have[..12],
            &bundle.base_tree[..12]
        )));
    }
    let mut im = Import { repo, changes: BTreeMap::new(), snaps: BTreeMap::new() };
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
        if diverged_at.is_none() {
            let got = tree_hash(&rendered(repo, &repo.current()?)?);
            if got != b.after_tree {
                diverged_at = Some(b.ix);
            }
        }
    }
    Ok(ImportReport { applied, diverged_at })
}
