//! O5 (the oracle set): replaying the op log from the root reproduces every snapshot. The replay
//! runs on a fresh store with no filesystem, so anything the verbs keep outside the op path
//! shows up as a divergence. Snapshot hashes include change ids, and `Branch`/`Merge` mint
//! theirs at run time, so equality is on **content** (`Snapshot::content_eq`) with ids mapped
//! as the replay goes.

use std::collections::BTreeMap;

use serde::Serialize;
use svc_core::engine::{add_def_at, delete, edit_def, inline, move_def, relocate, rename};
use svc_core::{
    ChangeId, Langs, MemStore, Op, OpIx, OpLogEntry, Result, Snapshot, SnapshotId, Store, View,
};

use crate::history::child_of;
use crate::merge::{lca, merged_snapshot};
use crate::repo::Repo;

#[derive(Clone, Debug, Serialize)]
pub struct ReplayReport {
    pub ops: usize,
    /// First op whose replayed result differs from the recorded `after.root`, if any.
    pub diverged_at: Option<OpIx>,
}

impl ReplayReport {
    pub fn ok(&self) -> bool {
        self.diverged_at.is_none()
    }
}

struct Replay<'a> {
    src: &'a dyn Store,
    store: MemStore,
    langs: &'a Langs,
    /// Recorded snapshot id → replayed snapshot id.
    snaps: BTreeMap<SnapshotId, SnapshotId>,
    changes: BTreeMap<ChangeId, ChangeId>,
}

impl Replay<'_> {
    fn change(&self, c: ChangeId) -> ChangeId {
        *self.changes.get(&c).unwrap_or(&c)
    }

    fn current(&self) -> Result<Snapshot> {
        self.store.get_snapshot(self.store.root()?)
    }

    fn commit(&self, snap: &Snapshot) -> Result<SnapshotId> {
        let id = self.store.put_snapshot(snap)?;
        self.store.set_head(snap.change, id)?;
        self.store.set_root(id)?;
        Ok(id)
    }

    fn amend(&self, cur: &Snapshot, mut next: Snapshot) -> Result<SnapshotId> {
        let cur_id = cur.id();
        if next.content_eq(cur) {
            return Ok(cur_id);
        }
        next.parents = cur.parents.clone();
        next.predecessors = vec![cur_id];
        next.change = cur.change;
        self.commit(&next)
    }

    /// Copy a recorded snapshot in (used for `Absorb`, whose result is data, not an op).
    fn import(&mut self, recorded: SnapshotId) -> Result<SnapshotId> {
        let mut snap = self.src.get_snapshot(recorded)?;
        snap.change = self.change(snap.change);
        snap.parents = snap.parents.iter().map(|p| *self.snaps.get(p).unwrap_or(p)).collect();
        snap.predecessors = snap.predecessors.iter().map(|p| *self.snaps.get(p).unwrap_or(p)).collect();
        for rec in snap.entities.values() {
            self.store.put_blob(&self.src.get_blob(&rec.content.0)?)?;
            self.store.put_blob(&self.src.get_blob(&rec.bytes.0)?)?;
        }
        self.commit(&snap)
    }

    fn restore(&self, view: &View) -> Result<()> {
        for (change, snap) in &view.heads {
            if let Some(mapped) = self.snaps.get(snap) {
                self.store.set_head(self.change(*change), *mapped)?;
            }
        }
        if let Some(root) = self.snaps.get(&view.root) {
            self.store.set_root(*root)?;
        }
        Ok(())
    }

    fn apply(&mut self, e: &OpLogEntry) -> Result<()> {
        // The entry records which root it started from. With several checkouts on one op
        // log (or a `checkout` between ops) that is not necessarily the last op's result:
        // switch to it first — a checkout switch, not a content change.
        if let Some(mapped) = self.snaps.get(&e.before.root).copied() {
            if mapped != self.store.root()? {
                self.store.set_root(mapped)?;
            }
        }
        let cur = self.current()?;
        match &e.op {
            Op::Rename { id, new } => {
                self.amend(&cur, rename(&cur, *id, new)?)?;
            }
            Op::Move { id, parent, ordinal } => {
                self.amend(&cur, move_def(&self.store, self.langs, &cur, *id, *parent, *ordinal)?)?;
            }
            Op::Relocate { id, file, ordinal } => {
                self.amend(&cur, relocate(&cur, *id, file.clone(), *ordinal)?)?;
            }
            Op::Extract { id, new_parent, ordinal } => {
                self.amend(&cur, move_def(&self.store, self.langs, &cur, *id, *new_parent, Some(*ordinal))?)?;
            }
            Op::Inline { id } => {
                self.amend(&cur, inline(&self.store, self.langs, &cur, *id)?)?;
            }
            Op::AddDef { id, parent, ordinal, definition, intent, file } => {
                let next = add_def_at(&self.store, self.langs, &cur, *id, *parent, file.clone(), *ordinal, definition.as_bytes(), intent.clone())?;
                self.amend(&cur, next)?;
            }
            Op::Delete { id, .. } => {
                self.amend(&cur, delete(&cur, &self.store, *id)?)?;
            }
            Op::EditDef { id, definition, .. } => {
                let (next, _) = edit_def(&self.store, self.langs, &cur, *id, definition.as_bytes())?;
                self.amend(&cur, next)?;
            }
            Op::New { change } => {
                let c = self.change(*change);
                self.commit(&child_of(&cur, cur.id(), c))?;
            }
            Op::Describe { msg } => {
                let mut next = cur.clone();
                next.message = msg.clone();
                self.amend(&cur, next)?;
            }
            Op::Branch { .. } => {
                let recorded = self.src.get_snapshot(e.after.root)?;
                let c = self.fresh_change(recorded.change);
                let (base, base_id) = match cur.parents.first() {
                    Some(p) => (self.store.get_snapshot(*p)?, *p),
                    None => (cur.clone(), cur.id()),
                };
                self.commit(&child_of(&base, base_id, c))?;
            }
            Op::Merge { other } => {
                let other_id = self.store.head(self.change(*other))?;
                let cur_id = cur.id();
                let base_id = lca(&self.store, cur_id, other_id)?
                    .ok_or_else(|| svc_core::Error::Other("replay: no common ancestor".into()))?;
                let recorded = self.src.get_snapshot(e.after.root)?;
                let c = self.fresh_change(recorded.change);
                let snap = merged_snapshot(&self.store, self.langs, base_id, cur_id, other_id, c)?;
                self.commit(&snap)?;
            }
            Op::Undo => self.restore(&e.after)?,
            Op::Absorb => {
                self.import(e.after.root)?;
            }
        }
        Ok(())
    }

    fn fresh_change(&mut self, recorded: ChangeId) -> ChangeId {
        let c = ChangeId::new();
        self.changes.insert(recorded, c);
        c
    }
}

/// Replay `repo`'s op log on a fresh store; report the first divergence.
pub fn replay(repo: &Repo) -> Result<ReplayReport> {
    let src = repo.store();
    let ops = src.ops(OpIx(0), false)?;
    let mut r = Replay {
        src,
        store: MemStore::new(),
        langs: repo.langs(),
        snaps: BTreeMap::new(),
        changes: BTreeMap::new(),
    };
    let mut diverged_at = None;
    for (i, (ix, e)) in ops.iter().enumerate() {
        if i == 0 {
            // The init op's `after.root` is the ingested tree; replay starts from it.
            r.import(e.after.root)?;
        } else {
            r.apply(e)?;
        }
        let got = r.current()?;
        let want = src.get_snapshot(e.after.root)?;
        r.snaps.insert(e.after.root, got.id());
        if !got.content_eq(&want) && diverged_at.is_none() {
            diverged_at = Some(*ix);
        }
    }
    Ok(ReplayReport {
        ops: ops.len(),
        diverged_at,
    })
}
