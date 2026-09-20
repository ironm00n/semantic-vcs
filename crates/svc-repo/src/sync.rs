//! A changeset travels between two stores as a bundle: `push` sends the ops of one group
//! that the other store does not have yet, `pull` is the same the other way round. The
//! receiving clone then lists the same changeset — its ops, their verdicts and subjects —
//! because the review state is the op log, not a server's table.

use std::mem::{Discriminant, discriminant};
use std::path::Path;

use svc_core::{ChangeSetId, Error, Op, OpIx, Result, Timestamp};

use crate::bundle::{self, ImportReport};
use crate::repo::Repo;

#[derive(Clone, Debug, serde::Serialize)]
pub struct SyncReport {
    pub changeset: ChangeSetId,
    /// Ops sent; 0 when the other side already had everything.
    pub sent: usize,
    pub import: Option<ImportReport>,
}

/// The ops of `group` a store holds, by time and kind: what a sync need not send. Not by
/// payload — an entity id is the receiving store's own, mapped by path on import.
fn held(repo: &Repo, group: ChangeSetId) -> Result<Vec<(Timestamp, Discriminant<Op>)>> {
    Ok(repo
        .store()
        .ops(OpIx(0), false)?
        .into_iter()
        .filter(|(_, e)| bundle::belongs(e, group))
        .map(|(_, e)| (e.at, discriminant(&e.op)))
        .collect())
}

/// Copy the ops of `group` that `to` does not have from `from` into `to`. Both stores must
/// stand on the same tree at the first op sent (a fresh clone, or a clone that received the
/// previous push and made no other change), as `history import` requires.
pub fn transfer(from: &Repo, to: &Repo, group: ChangeSetId) -> Result<SyncReport> {
    from.store().get_changeset(group)?;
    let have = held(to, group)?;
    let b = bundle::export_changeset(from, group, |e| have.contains(&(e.at, discriminant(&e.op))))?;
    if b.entries.is_empty() {
        return Ok(SyncReport { changeset: group, sent: 0, import: None });
    }
    let sent = b.entries.len();
    let import = bundle::import(to, &b)?;
    Ok(SyncReport { changeset: group, sent, import: Some(import) })
}

/// The other checkout of `svc push`/`svc pull`. Opened once: a second open of the same
/// checkout in one process would wait on its own lock.
pub fn open_checkout(dir: &Path) -> Result<Repo> {
    Repo::open(dir, Repo::default_langs()).map_err(|e| Error::Other(format!("{}: {e}", dir.display())))
}
