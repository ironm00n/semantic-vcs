//! A changeset travels between two stores as a bundle: `push` sends the ops of one group
//! that the other store does not have yet, `pull` is the same the other way round. The
//! receiving clone then lists the same changeset — its ops, their verdicts and subjects —
//! because the review state is the op log, not a server's table.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;
use svc_core::{ChangeSetId, Error, OpIx, OpLogEntry, Result, Snapshot};

use crate::bundle::{self, ImportReport};
use crate::history::parse_entity_id;
use crate::repo::Repo;

#[derive(Clone, Debug, serde::Serialize)]
pub struct SyncReport {
    pub changeset: ChangeSetId,
    /// Ops sent; 0 when the other side already had everything.
    pub sent: usize,
    pub import: Option<ImportReport>,
}

/// An op's identity across stores: its time and its payload with every entity id replaced
/// by the entity's file and name as the op's own before-snapshot had them (an id is each
/// store's own; a change id or an id the snapshot lacks becomes `?` on both sides). Two
/// ops of one kind in one millisecond still tell apart by what they did.
fn fingerprint(repo: &Repo, e: &OpLogEntry) -> String {
    fn portable(v: &mut Value, snap: Option<&Snapshot>) {
        match v {
            Value::String(s) => {
                if let Some(id) = parse_entity_id(s) {
                    *s = snap
                        .and_then(|sn| sn.entities.get(&id))
                        .map(|r| format!("{}:{}", r.file, r.name))
                        .unwrap_or_else(|| "?".into());
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| portable(x, snap)),
            Value::Object(m) => m.values_mut().for_each(|x| portable(x, snap)),
            _ => {}
        }
    }
    let before = repo.store().get_snapshot(e.before.root).ok();
    let mut v = serde_json::to_value(&e.op).unwrap_or(Value::Null);
    portable(&mut v, before.as_ref());
    format!("{}:{v}", e.at)
}

/// The ops of `group` a store holds: what a sync need not send.
fn held(repo: &Repo, group: ChangeSetId) -> Result<BTreeSet<String>> {
    Ok(repo
        .store()
        .ops(OpIx(0), false)?
        .iter()
        .filter(|(_, e)| bundle::belongs(e, group))
        .map(|(_, e)| fingerprint(repo, e))
        .collect())
}

/// Copy the ops of `group` that `to` does not have from `from` into `to`. Both stores must
/// stand on the same tree at the first op sent (a fresh clone, or a clone that received the
/// previous push and made no other change), as `history import` requires.
pub fn transfer(from: &Repo, to: &Repo, group: ChangeSetId, sender: &str) -> Result<SyncReport> {
    from.store().get_changeset(group)?;
    let have = held(to, group)?;
    let b = bundle::export_changeset(from, group, |e| have.contains(&fingerprint(from, e)))?;
    if b.entries.is_empty() {
        return Ok(SyncReport { changeset: group, sent: 0, import: None });
    }
    let sent = b.entries.len();
    // Ops of a primary checkout carry no checkout name; the other store needs one to say
    // who wrote a note and to address the answer.
    let mut b = b;
    for entry in &mut b.entries {
        entry.workspace.get_or_insert_with(|| sender.to_string());
    }
    let import = bundle::import(to, &b)?;
    Ok(SyncReport { changeset: group, sent, import: Some(import) })
}

/// The other checkout of `svc push`/`svc pull`. Opened once: a second open of the same
/// checkout in one process would wait on its own lock.
pub fn open_checkout(dir: &Path) -> Result<Repo> {
    Repo::open(dir, Repo::default_langs()).map_err(|e| Error::Other(format!("{}: {e}", dir.display())))
}

/// What the other checkout is called from here: its workspace name, else its directory.
/// (`checkout_name` reads `SVC_CHECKOUT`, which names this process's own checkout.)
pub fn remote_name(other: &Repo) -> String {
    other
        .workspace()
        .map(str::to_string)
        .or_else(|| other.root_dir().file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "remote".into())
}
