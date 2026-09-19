//! `svc_core::Store` over one redb file. Snapshots and blobs are content-addressed and
//! immutable; only `root`, `heads`, branches and the changeset rows ever move.

use std::path::Path;

use redb::{
    Database, ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction,
};
use serde::{Serialize, de::DeserializeOwned};
use svc_core::{
    ChangeId, ChangeSet, ChangeSetId, Error, OpIx, OpLogEntry, OpenChangeSet, Result, Snapshot,
    SnapshotId, Store,
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

const META_ROOT: &str = "root";
const META_OPEN_CHANGESET: &str = "open_changeset";
const META_RENDER_PENDING: &str = "render_pending";

pub struct RedbStore {
    db: Database,
}

impl RedbStore {
    /// Creates the file and seeds every table, so readers never see `TableDoesNotExist`.
    pub fn create(path: &Path) -> Result<Self> {
        let db = Database::create(path).map_err(Error::backend)?;
        let store = Self { db };
        store.write(|txn| {
            txn.open_table(OBJECTS).map_err(Error::backend)?;
            txn.open_table(SNAPSHOTS).map_err(Error::backend)?;
            txn.open_table(OPLOG).map_err(Error::backend)?;
            txn.open_table(HEADS).map_err(Error::backend)?;
            txn.open_table(CHANGESETS).map_err(Error::backend)?;
            txn.open_table(BRANCHES).map_err(Error::backend)?;
            txn.open_table(META).map_err(Error::backend)?;
            Ok(())
        })?;
        Ok(store)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::open(path).map_err(Error::backend)?;
        Ok(Self { db })
    }

    /// True when the failure is redb's cross-process lock, the one case worth retrying.
    pub fn is_already_open(err: &Error) -> bool {
        matches!(err, Error::Backend(e) if e.to_string().contains("already open"))
    }

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
        self.write(|txn| {
            let mut table = txn.open_table(OPLOG).map_err(Error::backend)?;
            let next = table
                .last()
                .map_err(Error::backend)?
                .map(|(k, _)| k.value() + 1)
                .unwrap_or(0);
            table.insert(next, bytes.as_slice()).map_err(Error::backend)?;
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
        let txn = self.read()?;
        let table = txn.open_table(HEADS).map_err(Error::backend)?;
        table
            .get(id.as_uuid())
            .map_err(Error::backend)?
            .map(|g| SnapshotId(*g.value()))
            .ok_or(Error::NoSuchChange(id))
    }

    fn set_head(&self, id: ChangeId, snap: SnapshotId) -> Result<()> {
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
        self.get_meta::<SnapshotId>(META_ROOT)?
            .ok_or(Error::NoSuchSnapshot)
    }

    fn set_root(&self, id: SnapshotId) -> Result<()> {
        self.set_meta(META_ROOT, &id)
    }

    fn open_changeset(&self) -> Result<Option<OpenChangeSet>> {
        Ok(self
            .get_meta::<Option<OpenChangeSet>>(META_OPEN_CHANGESET)?
            .flatten())
    }

    fn set_open_changeset(&self, row: Option<OpenChangeSet>) -> Result<()> {
        self.set_meta(META_OPEN_CHANGESET, &row)
    }

    fn render_pending(&self) -> Result<bool> {
        Ok(self
            .get_meta::<bool>(META_RENDER_PENDING)?
            .unwrap_or(false))
    }

    fn set_render_pending(&self, v: bool) -> Result<()> {
        self.set_meta(META_RENDER_PENDING, &v)
    }
}
