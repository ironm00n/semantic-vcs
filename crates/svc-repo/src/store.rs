//! `svc_core::Store` over one redb file. Snapshots and blobs are content-addressed and
//! immutable; only `root`, `heads`, branches and the changeset rows ever move.
//!
//! One file can back several checkouts (jj-style named workspaces, see `workspace.rs`).
//! Snapshots, heads, branches and the op log are shared; the checkout-scoped rows — `root`,
//! `render_pending`, `open_changeset` — live in `META` for the default checkout and in a
//! [`WorkspaceRow`] for a named one. Which set a `RedbStore` reads is fixed at open time.

use std::path::{Path, PathBuf};

use redb::{
    ConcurrencyMode, Database, ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition,
    TableError, WriteTransaction,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use svc_core::{
    ChangeId, ChangeSet, ChangeSetId, Error, OpIx, OpLogEntry, OpenChangeSet, Result, Snapshot,
    SnapshotId, Store, View,
};
use uuid::Uuid;

const OBJECTS: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("objects");
const SNAPSHOTS: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("snapshots");
const OPLOG: TableDefinition<u64, &[u8]> = TableDefinition::new("oplog");
const HEADS: TableDefinition<Uuid, &[u8; 32]> = TableDefinition::new("heads");
const CHANGESETS: TableDefinition<Uuid, &[u8]> = TableDefinition::new("changesets");
const BRANCHES: TableDefinition<&str, Uuid> = TableDefinition::new("branches");
/// Singleton rows: `root`, `open_changeset`, `render_pending`.
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// Named checkouts: `name → WorkspaceRow`. The default checkout is not a row here.
const WORKSPACES: TableDefinition<&str, &[u8]> = TableDefinition::new("workspaces");
/// Which checkout appended each op (`OpIx → name`; the default checkout writes `""`).
/// `OpLogEntry` is a frozen `svc-core` shape, so attribution lives beside it, not in it.
const OP_WORKSPACE: TableDefinition<u64, &str> = TableDefinition::new("op_workspace");

const META_ROOT: &str = "root";
const META_OPEN_CHANGESET: &str = "open_changeset";
const META_RENDER_PENDING: &str = "render_pending";

/// Everything a named checkout keeps that the default checkout keeps in `META`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    /// Absolute directory the checkout is rendered into.
    pub path: PathBuf,
    pub root: Option<SnapshotId>,
    pub render_pending: bool,
    pub open_changeset: Option<OpenChangeSet>,
}

/// Head/root/render-pending writes held back until `append_op`'s transaction, so a
/// mutation is published in one redb txn and a crash can never leave head and root a
/// snapshot ahead of the op log. Reads consult it first, so the code in
/// between (`amend`, `view`) sees what it just wrote.
///
/// `expected` is the view the verb computed from. Other processes share the store, so
/// `append_op` compares the persisted root and every head it is about to move against it
/// inside the write transaction: two processes that both read head H and both try to
/// publish H→H' cannot both win — the second finds H' and is refused with nothing written.
struct Staged {
    expected: View,
    heads: std::collections::BTreeMap<ChangeId, SnapshotId>,
    root: Option<SnapshotId>,
    render_pending: Option<bool>,
}

pub struct RedbStore {
    db: Database,
    /// `None` = the default checkout (`META` rows); `Some(name)` = a `WORKSPACES` row.
    workspace: Option<String>,
    staged: std::sync::Mutex<Option<Staged>>,
}

impl RedbStore {
    /// `MultiWriter`: opens never exclude each other (a TUI session and CLI verbs on other
    /// checkouts share one store); each write transaction takes a byte-range lock on the file
    /// and readers follow commits. Linux, macOS and Windows only.
    fn database(path: &Path, create: bool) -> Result<Database> {
        let mut builder = Database::builder();
        builder.set_concurrency_mode(ConcurrencyMode::MultiWriter);
        if create {
            builder.create(path)
        } else {
            builder.open(path)
        }
        .map_err(Error::backend)
    }

    /// Creates the file and seeds every table, so readers never see `TableDoesNotExist`.
    pub fn create(path: &Path) -> Result<Self> {
        let db = Self::database(path, true)?;
        let store = Self { db, workspace: None, staged: Default::default() };
        store.write(|txn| {
            txn.open_table(OBJECTS).map_err(Error::backend)?;
            txn.open_table(SNAPSHOTS).map_err(Error::backend)?;
            txn.open_table(OPLOG).map_err(Error::backend)?;
            txn.open_table(HEADS).map_err(Error::backend)?;
            txn.open_table(CHANGESETS).map_err(Error::backend)?;
            txn.open_table(BRANCHES).map_err(Error::backend)?;
            txn.open_table(META).map_err(Error::backend)?;
            txn.open_table(WORKSPACES).map_err(Error::backend)?;
            txn.open_table(OP_WORKSPACE).map_err(Error::backend)?;
            Ok(())
        })?;
        Ok(store)
    }

    /// The checkout that appended op `ix`: `Some("")` for the default one, `None` for ops
    /// written before attribution existed.
    pub fn op_workspace(&self, ix: OpIx) -> Result<Option<String>> {
        let txn = self.read()?;
        let table = match txn.open_table(OP_WORKSPACE) {
            Ok(t) => t,
            Err(TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(Error::backend(e)),
        };
        Ok(table
            .get(ix.0)
            .map_err(Error::backend)?
            .map(|g| g.value().to_string()))
    }

    /// `ops(since, rev)` restricted to what this handle's checkout appended (unattributed
    /// ops count as the default checkout's).
    pub fn own_ops(&self, since: OpIx, rev: bool) -> Result<Vec<(OpIx, OpLogEntry)>> {
        let me = self.workspace.clone().unwrap_or_default();
        let mut out = Vec::new();
        for (ix, e) in self.ops(since, rev)? {
            if self.op_workspace(ix)?.unwrap_or_default() == me {
                out.push((ix, e));
            }
        }
        Ok(out)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let db = Self::database(path, false)?;
        Ok(Self { db, workspace: None, staged: Default::default() })
    }

    /// Re-scope this handle to checkout `name` (`None` = default). The row must exist.
    pub fn with_workspace(mut self, name: Option<&str>) -> Result<Self> {
        if let Some(n) = name {
            self.workspace_row(n)?
                .ok_or_else(|| Error::NotFound(format!("workspace {n:?}")))?;
        }
        self.workspace = name.map(str::to_string);
        Ok(self)
    }

    /// Hold head/root/render-pending writes until the next `append_op`, which publishes
    /// them with the op in one transaction. `Repo::mutate` brackets every verb with this.
    pub fn stage(&self, expected: &View) {
        *self.staged.lock().unwrap() = Some(Staged {
            expected: expected.clone(),
            heads: Default::default(),
            root: None,
            render_pending: None,
        });
    }

    /// Drop everything staged since `stage()` without writing it (the verb failed;
    /// snapshots already put are content-addressed orphans and harmless).
    pub fn discard_staged(&self) {
        *self.staged.lock().unwrap() = None;
    }

    fn staged_head(&self, id: ChangeId) -> Option<SnapshotId> {
        self.staged.lock().unwrap().as_ref().and_then(|s| s.heads.get(&id).copied())
    }

    fn staged_root(&self) -> Option<SnapshotId> {
        self.staged.lock().unwrap().as_ref().and_then(|s| s.root)
    }

    fn staged_render_pending(&self) -> Option<bool> {
        self.staged.lock().unwrap().as_ref().and_then(|s| s.render_pending)
    }

    /// Record instead of writing when staging is on. Returns whether it was recorded.
    fn stage_with(&self, f: impl FnOnce(&mut Staged)) -> bool {
        match self.staged.lock().unwrap().as_mut() {
            Some(s) => {
                f(s);
                true
            }
            None => false,
        }
    }

    /// A checkout-row update inside an open write transaction.
    fn update_own_row_in(&self, txn: &WriteTransaction, name: &str, f: impl FnOnce(&mut WorkspaceRow)) -> Result<()> {
        let mut table = txn.open_table(WORKSPACES).map_err(Error::backend)?;
        let mut row: WorkspaceRow = match table.get(name).map_err(Error::backend)? {
            Some(g) => decode(g.value())?,
            None => return Err(Error::NotFound(format!("workspace {name:?}"))),
        };
        f(&mut row);
        let bytes = encode(&row)?;
        table.insert(name, bytes.as_slice()).map_err(Error::backend)?;
        Ok(())
    }

    fn set_meta_in<T: Serialize>(&self, txn: &WriteTransaction, key: &str, value: &T) -> Result<()> {
        let bytes = encode(value)?;
        let mut table = txn.open_table(META).map_err(Error::backend)?;
        table.insert(key, bytes.as_slice()).map_err(Error::backend)?;
        Ok(())
    }

    /// This checkout's persisted `root`, read inside an open write transaction (never staged).
    fn root_in(&self, txn: &WriteTransaction) -> Result<Option<SnapshotId>> {
        match &self.workspace {
            None => {
                let table = txn.open_table(META).map_err(Error::backend)?;
                match table.get(META_ROOT).map_err(Error::backend)? {
                    Some(g) => Ok(Some(decode(g.value())?)),
                    None => Ok(None),
                }
            }
            Some(name) => {
                let table = txn.open_table(WORKSPACES).map_err(Error::backend)?;
                match table.get(name.as_str()).map_err(Error::backend)? {
                    Some(g) => Ok(decode::<WorkspaceRow>(g.value())?.root),
                    None => Err(Error::NotFound(format!("workspace {name:?}"))),
                }
            }
        }
    }

    /// The checkout this handle's `root`/`render_pending`/`open_changeset` refer to.
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    /// Every named checkout, sorted by name. Stores created before the table existed read
    /// as empty rather than failing.
    pub fn workspaces(&self) -> Result<Vec<(String, WorkspaceRow)>> {
        let txn = self.read()?;
        let table = match txn.open_table(WORKSPACES) {
            Ok(t) => t,
            Err(TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(Error::backend(e)),
        };
        let mut out = Vec::new();
        for row in table.iter().map_err(Error::backend)? {
            let (k, v) = row.map_err(Error::backend)?;
            out.push((k.value().to_string(), decode(v.value())?));
        }
        Ok(out)
    }

    pub fn workspace_row(&self, name: &str) -> Result<Option<WorkspaceRow>> {
        Ok(self
            .workspaces()?
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, row)| row))
    }

    pub fn set_workspace_row(&self, name: &str, row: &WorkspaceRow) -> Result<()> {
        let bytes = encode(row)?;
        self.write(|txn| {
            let mut table = txn.open_table(WORKSPACES).map_err(Error::backend)?;
            table.insert(name, bytes.as_slice()).map_err(Error::backend)?;
            Ok(())
        })
    }

    /// Drops the row; returns whether it existed. The files on disk are untouched.
    pub fn remove_workspace(&self, name: &str) -> Result<bool> {
        self.write(|txn| {
            let mut table = txn.open_table(WORKSPACES).map_err(Error::backend)?;
            Ok(table.remove(name).map_err(Error::backend)?.is_some())
        })
    }

    /// The default checkout's `root`, whatever this handle is scoped to.
    pub fn default_root(&self) -> Result<Option<SnapshotId>> {
        self.get_meta::<SnapshotId>(META_ROOT)
    }

    /// This handle's row, when scoped to a named checkout.
    fn own_row(&self, name: &str) -> Result<WorkspaceRow> {
        self.workspace_row(name)?
            .ok_or_else(|| Error::NotFound(format!("workspace {name:?}")))
    }

    fn update_own_row(&self, name: &str, f: impl FnOnce(&mut WorkspaceRow)) -> Result<()> {
        let mut row = self.own_row(name)?;
        f(&mut row);
        self.set_workspace_row(name, &row)
    }

    /// True when the failure is redb's cross-process lock, the one case worth retrying.
    fn read(&self) -> Result<ReadTransaction> {
        self.db.begin_read().map_err(Error::backend)
    }

    fn write<R>(&self, f: impl FnOnce(&WriteTransaction) -> Result<R>) -> Result<R> {
        let txn = self.db.begin_write().map_err(Error::backend)?;
        let out = f(&txn)?;
        txn.commit().map_err(Error::backend)?;
        Ok(out)
    }

    fn get_meta<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let txn = self.read()?;
        let table = txn.open_table(META).map_err(Error::backend)?;
        match table.get(key).map_err(Error::backend)? {
            Some(g) => Ok(Some(decode(g.value())?)),
            None => Ok(None),
        }
    }

    fn set_meta<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let bytes = encode(value)?;
        self.write(|txn| {
            let mut table = txn.open_table(META).map_err(Error::backend)?;
            table.insert(key, bytes.as_slice()).map_err(Error::backend)?;
            Ok(())
        })
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_stdvec(value).map_err(|e| Error::Other(e.to_string()))
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    postcard::from_bytes(bytes).map_err(|e| Error::Other(e.to_string()))
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Store for RedbStore {
    fn put_blob(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        let id = *blake3::hash(bytes).as_bytes();
        self.write(|txn| {
            let mut table = txn.open_table(OBJECTS).map_err(Error::backend)?;
            table.insert(&id, bytes).map_err(Error::backend)?;
            Ok(())
        })?;
        Ok(id)
    }

    fn get_blob(&self, id: &[u8; 32]) -> Result<Vec<u8>> {
        let txn = self.read()?;
        let table = txn.open_table(OBJECTS).map_err(Error::backend)?;
        table
            .get(id)
            .map_err(Error::backend)?
            .map(|g| g.value().to_vec())
            .ok_or_else(|| Error::NotFound(hex32(id)))
    }

    fn put_snapshot(&self, s: &Snapshot) -> Result<SnapshotId> {
        let id = s.id();
        let bytes = encode(s)?;
        self.write(|txn| {
            let mut table = txn.open_table(SNAPSHOTS).map_err(Error::backend)?;
            table.insert(&id.0, bytes.as_slice()).map_err(Error::backend)?;
            Ok(())
        })?;
        Ok(id)
    }

    fn get_snapshot(&self, id: SnapshotId) -> Result<Snapshot> {
        let txn = self.read()?;
        let table = txn.open_table(SNAPSHOTS).map_err(Error::backend)?;
        match table.get(&id.0).map_err(Error::backend)? {
            Some(g) => decode(g.value()),
            None => Err(Error::NoSuchSnapshot),
        }
    }

    fn append_op(&self, e: &OpLogEntry) -> Result<OpIx> {
        let bytes = encode(e)?;
        let ws = self.workspace.clone().unwrap_or_default();
        let staged = self.staged.lock().unwrap().take();
        self.write(|txn| {
            // Publish what the verb staged together with the entry that describes it.
            if let Some(s) = &staged {
                // Compare-and-swap against the view the verb read: another process may have
                // published since. A mismatch aborts the transaction with nothing written.
                if self.root_in(txn)? != Some(s.expected.root) {
                    return Err(Error::Other(
                        "concurrent update: this checkout's root moved under the verb; nothing was written"
                            .into(),
                    ));
                }
                if !s.heads.is_empty() {
                    let mut heads = txn.open_table(HEADS).map_err(Error::backend)?;
                    for (c, snap) in &s.heads {
                        let actual = heads
                            .get(c.as_uuid())
                            .map_err(Error::backend)?
                            .map(|g| SnapshotId(*g.value()));
                        if actual != s.expected.heads.get(c).copied() {
                            return Err(Error::Other(format!(
                                "concurrent update: change {} moved to {} under the verb; nothing was written (run `svc workspace update-stale`)",
                                c.short(),
                                actual.map(|a| a.short()).unwrap_or_else(|| "nothing".into())
                            )));
                        }
                        heads.insert(c.as_uuid(), &snap.0).map_err(Error::backend)?;
                    }
                }
                match &self.workspace {
                    None => {
                        if let Some(r) = s.root {
                            self.set_meta_in(txn, META_ROOT, &r)?;
                        }
                        if let Some(p) = s.render_pending {
                            self.set_meta_in(txn, META_RENDER_PENDING, &p)?;
                        }
                    }
                    Some(name) if s.root.is_some() || s.render_pending.is_some() => {
                        self.update_own_row_in(txn, name, |r| {
                            if let Some(id) = s.root {
                                r.root = Some(id);
                            }
                            if let Some(p) = s.render_pending {
                                r.render_pending = p;
                            }
                        })?;
                    }
                    Some(_) => {}
                }
            }
            let mut table = txn.open_table(OPLOG).map_err(Error::backend)?;
            let next = table
                .last()
                .map_err(Error::backend)?
                .map(|(k, _)| k.value() + 1)
                .unwrap_or(0);
            table.insert(next, bytes.as_slice()).map_err(Error::backend)?;
            let mut by = txn.open_table(OP_WORKSPACE).map_err(Error::backend)?;
            by.insert(next, ws.as_str()).map_err(Error::backend)?;
            Ok(OpIx(next))
        })
    }

    fn ops(&self, since: OpIx, rev: bool) -> Result<Vec<(OpIx, OpLogEntry)>> {
        let txn = self.read()?;
        let table = txn.open_table(OPLOG).map_err(Error::backend)?;
        let mut out = Vec::new();
        for row in table.range(since.0..).map_err(Error::backend)? {
            let (k, v) = row.map_err(Error::backend)?;
            out.push((OpIx(k.value()), decode(v.value())?));
        }
        if rev {
            out.reverse();
        }
        Ok(out)
    }

    fn head(&self, id: ChangeId) -> Result<SnapshotId> {
        if let Some(s) = self.staged_head(id) {
            return Ok(s);
        }
        let txn = self.read()?;
        let table = txn.open_table(HEADS).map_err(Error::backend)?;
        table
            .get(id.as_uuid())
            .map_err(Error::backend)?
            .map(|g| SnapshotId(*g.value()))
            .ok_or(Error::NoSuchChange(id))
    }

    fn set_head(&self, id: ChangeId, snap: SnapshotId) -> Result<()> {
        if self.stage_with(|s| {
            s.heads.insert(id, snap);
        }) {
            return Ok(());
        }
        self.write(|txn| {
            let mut table = txn.open_table(HEADS).map_err(Error::backend)?;
            table
                .insert(id.as_uuid(), &snap.0)
                .map_err(Error::backend)?;
            Ok(())
        })
    }

    fn heads(&self) -> Result<Vec<(ChangeId, SnapshotId)>> {
        let txn = self.read()?;
        let table = txn.open_table(HEADS).map_err(Error::backend)?;
        let mut out = Vec::new();
        for row in table.iter().map_err(Error::backend)? {
            let (k, v) = row.map_err(Error::backend)?;
            out.push((ChangeId(k.value()), SnapshotId(*v.value())));
        }
        if let Some(s) = self.staged.lock().unwrap().as_ref() {
            for (c, snap) in &s.heads {
                match out.iter_mut().find(|(k, _)| k == c) {
                    Some(slot) => slot.1 = *snap,
                    None => out.push((*c, *snap)),
                }
            }
        }
        Ok(out)
    }

    fn evolog(&self, id: ChangeId) -> Result<Vec<Snapshot>> {
        let mut cur = self.head(id)?;
        let mut out = Vec::new();
        loop {
            let snap = self.get_snapshot(cur)?;
            let next = snap.predecessors.first().copied();
            out.push(snap);
            match next {
                Some(p) => cur = p,
                None => break,
            }
        }
        Ok(out)
    }

    fn put_changeset(&self, cs: &ChangeSet) -> Result<()> {
        let bytes = encode(cs)?;
        self.write(|txn| {
            let mut table = txn.open_table(CHANGESETS).map_err(Error::backend)?;
            table
                .insert(cs.id.as_uuid(), bytes.as_slice())
                .map_err(Error::backend)?;
            Ok(())
        })
    }

    fn get_changeset(&self, id: ChangeSetId) -> Result<ChangeSet> {
        let txn = self.read()?;
        let table = txn.open_table(CHANGESETS).map_err(Error::backend)?;
        match table.get(id.as_uuid()).map_err(Error::backend)? {
            Some(g) => decode(g.value()),
            None => Err(Error::NotFound(id.to_string())),
        }
    }

    fn changesets(&self) -> Result<Vec<ChangeSet>> {
        let txn = self.read()?;
        let table = txn.open_table(CHANGESETS).map_err(Error::backend)?;
        let mut out = Vec::new();
        for row in table.iter().map_err(Error::backend)? {
            let (_, v) = row.map_err(Error::backend)?;
            out.push(decode(v.value())?);
        }
        Ok(out)
    }

    fn set_branch(&self, name: &str, id: ChangeId) -> Result<()> {
        self.write(|txn| {
            let mut table = txn.open_table(BRANCHES).map_err(Error::backend)?;
            table.insert(name, id.as_uuid()).map_err(Error::backend)?;
            Ok(())
        })
    }

    fn branch(&self, name: &str) -> Result<Option<ChangeId>> {
        let txn = self.read()?;
        let table = txn.open_table(BRANCHES).map_err(Error::backend)?;
        Ok(table
            .get(name)
            .map_err(Error::backend)?
            .map(|g| ChangeId(g.value())))
    }

    fn branches(&self) -> Result<Vec<(String, ChangeId)>> {
        let txn = self.read()?;
        let table = txn.open_table(BRANCHES).map_err(Error::backend)?;
        let mut out = Vec::new();
        for row in table.iter().map_err(Error::backend)? {
            let (k, v) = row.map_err(Error::backend)?;
            out.push((k.value().to_string(), ChangeId(v.value())));
        }
        Ok(out)
    }

    fn resolve_prefix(&self, prefix: &str) -> Result<ChangeId> {
        let p = prefix.to_ascii_lowercase();
        let hex = p.replace('-', "");
        let mut hits: Vec<ChangeId> = self
            .heads()?
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| id.short().starts_with(&p) || id.to_string().replace('-', "").starts_with(&hex))
            .collect();
        hits.sort();
        hits.dedup();
        match hits.len() {
            1 => Ok(hits[0]),
            0 => Err(Error::NotFound(format!("prefix {prefix}"))),
            _ => Err(Error::AmbiguousPrefix {
                prefix: prefix.into(),
                candidates: hits,
            }),
        }
    }

    fn root(&self) -> Result<SnapshotId> {
        if let Some(r) = self.staged_root() {
            return Ok(r);
        }
        match &self.workspace {
            None => self.get_meta::<SnapshotId>(META_ROOT)?,
            Some(ws) => self.own_row(ws)?.root,
        }
        .ok_or(Error::NoSuchSnapshot)
    }

    fn set_root(&self, id: SnapshotId) -> Result<()> {
        if self.stage_with(|s| s.root = Some(id)) {
            return Ok(());
        }
        match &self.workspace {
            None => self.set_meta(META_ROOT, &id),
            Some(ws) => self.update_own_row(ws, |r| r.root = Some(id)),
        }
    }

    fn open_changeset(&self) -> Result<Option<OpenChangeSet>> {
        match &self.workspace {
            None => Ok(self
                .get_meta::<Option<OpenChangeSet>>(META_OPEN_CHANGESET)?
                .flatten()),
            Some(ws) => Ok(self.own_row(ws)?.open_changeset),
        }
    }

    fn set_open_changeset(&self, row: Option<OpenChangeSet>) -> Result<()> {
        match &self.workspace {
            None => self.set_meta(META_OPEN_CHANGESET, &row),
            Some(ws) => self.update_own_row(ws, |r| r.open_changeset = row),
        }
    }

    fn render_pending(&self) -> Result<bool> {
        if let Some(p) = self.staged_render_pending() {
            return Ok(p);
        }
        match &self.workspace {
            None => Ok(self
                .get_meta::<bool>(META_RENDER_PENDING)?
                .unwrap_or(false)),
            Some(ws) => Ok(self.own_row(ws)?.render_pending),
        }
    }

    fn set_render_pending(&self, v: bool) -> Result<()> {
        if self.stage_with(|s| s.render_pending = Some(v)) {
            return Ok(());
        }
        match &self.workspace {
            None => self.set_meta(META_RENDER_PENDING, &v),
            Some(ws) => self.update_own_row(ws, |r| r.render_pending = v),
        }
    }
}
