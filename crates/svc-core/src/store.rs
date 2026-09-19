use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::changeset::{ChangeSet, OpenChangeSet};
use crate::content::{Bytes, Content};
use crate::error::{Error, Result};
use crate::ids::{
    BytesId, ChangeId, ChangeSetId, ContentId, OpIx, SnapshotId,
};
use crate::op::OpLogEntry;
use crate::snapshot::Snapshot;

pub trait Store: Send + Sync {
    fn put_blob(&self, bytes: &[u8]) -> Result<[u8; 32]>;
    fn get_blob(&self, id: &[u8; 32]) -> Result<Vec<u8>>;

    fn put_content(&self, c: &Content) -> Result<ContentId> {
        let id = c.id();
        let bytes = postcard::to_stdvec(c).map_err(|e| Error::Other(e.to_string()))?;
        self.put_blob(&bytes)?;
        let _ = id;
        Ok(id)
    }

    fn get_content(&self, id: ContentId) -> Result<Content> {
        let bytes = self.get_blob(&id.0)?;
        postcard::from_bytes(&bytes).map_err(|e| Error::Other(e.to_string()))
    }

    fn put_bytes_blob(&self, b: &Bytes) -> Result<BytesId> {
        let id = b.id();
        let bytes = postcard::to_stdvec(b).map_err(|e| Error::Other(e.to_string()))?;
        self.put_blob(&bytes)?;
        Ok(id)
    }

    fn get_bytes_blob(&self, id: BytesId) -> Result<Bytes> {
        let bytes = self.get_blob(&id.0)?;
        postcard::from_bytes(&bytes).map_err(|e| Error::Other(e.to_string()))
    }

    fn put_snapshot(&self, s: &Snapshot) -> Result<SnapshotId>;
    fn get_snapshot(&self, id: SnapshotId) -> Result<Snapshot>;

    fn append_op(&self, e: &OpLogEntry) -> Result<OpIx>;
    fn ops(&self, since: OpIx, rev: bool) -> Result<Vec<(OpIx, OpLogEntry)>>;

    fn head(&self, id: ChangeId) -> Result<SnapshotId>;
    fn set_head(&self, id: ChangeId, snap: SnapshotId) -> Result<()>;
    fn heads(&self) -> Result<Vec<(ChangeId, SnapshotId)>>;
    fn evolog(&self, id: ChangeId) -> Result<Vec<Snapshot>>;

    fn put_changeset(&self, cs: &ChangeSet) -> Result<()>;
    fn get_changeset(&self, id: ChangeSetId) -> Result<ChangeSet>;
    fn changesets(&self) -> Result<Vec<ChangeSet>>;

    fn set_branch(&self, name: &str, id: ChangeId) -> Result<()>;
    fn branch(&self, name: &str) -> Result<Option<ChangeId>>;
    fn branches(&self) -> Result<Vec<(String, ChangeId)>>;

    fn resolve_prefix(&self, prefix: &str) -> Result<ChangeId>;

    fn root(&self) -> Result<SnapshotId>;
    fn set_root(&self, id: SnapshotId) -> Result<()>;

    fn open_changeset(&self) -> Result<Option<OpenChangeSet>>;
    fn set_open_changeset(&self, row: Option<OpenChangeSet>) -> Result<()>;

    fn render_pending(&self) -> Result<bool>;
    fn set_render_pending(&self, v: bool) -> Result<()>;
}

#[derive(Default)]
struct MemInner {
    blobs: BTreeMap<[u8; 32], Vec<u8>>,
    snapshots: BTreeMap<[u8; 32], Snapshot>,
    oplog: Vec<OpLogEntry>,
    heads: BTreeMap<ChangeId, SnapshotId>,
    root: Option<SnapshotId>,
    changesets: BTreeMap<ChangeSetId, ChangeSet>,
    branches: BTreeMap<String, ChangeId>,
    open_changeset: Option<OpenChangeSet>,
    render_pending: bool,
}

#[derive(Default)]
pub struct MemStore {
    inner: Mutex<MemInner>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MemInner>> {
        self.inner
            .lock()
            .map_err(|_| Error::Other("memstore poisoned".into()))
    }
}

impl Store for MemStore {
    fn put_blob(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        let id = *blake3::hash(bytes).as_bytes();
        self.lock()?.blobs.insert(id, bytes.to_vec());
        Ok(id)
    }

    fn get_blob(&self, id: &[u8; 32]) -> Result<Vec<u8>> {
        self.lock()?
            .blobs
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(hex32(id)))
    }

    fn put_snapshot(&self, s: &Snapshot) -> Result<SnapshotId> {
        let id = s.id();
        self.lock()?.snapshots.insert(id.0, s.clone());
        Ok(id)
    }

    fn get_snapshot(&self, id: SnapshotId) -> Result<Snapshot> {
        self.lock()?
            .snapshots
            .get(&id.0)
            .cloned()
            .ok_or(Error::NoSuchSnapshot)
    }

    fn append_op(&self, e: &OpLogEntry) -> Result<OpIx> {
        let mut g = self.lock()?;
        g.oplog.push(e.clone());
        Ok(OpIx((g.oplog.len() - 1) as u64))
    }

    fn ops(&self, since: OpIx, rev: bool) -> Result<Vec<(OpIx, OpLogEntry)>> {
        let g = self.lock()?;
        let start = since.0 as usize;
        let mut out: Vec<(OpIx, OpLogEntry)> = g
            .oplog
            .iter()
            .enumerate()
            .skip(start)
            .map(|(i, e)| (OpIx(i as u64), e.clone()))
            .collect();
        if rev {
            out.reverse();
        }
        Ok(out)
    }

    fn head(&self, id: ChangeId) -> Result<SnapshotId> {
        self.lock()?
            .heads
            .get(&id)
            .copied()
            .ok_or(Error::NoSuchChange(id))
    }

    fn set_head(&self, id: ChangeId, snap: SnapshotId) -> Result<()> {
        self.lock()?.heads.insert(id, snap);
        Ok(())
    }

    fn heads(&self) -> Result<Vec<(ChangeId, SnapshotId)>> {
        Ok(self.lock()?.heads.iter().map(|(k, v)| (*k, *v)).collect())
    }

    fn evolog(&self, id: ChangeId) -> Result<Vec<Snapshot>> {
        let g = self.lock()?;
        let mut cur = *g.heads.get(&id).ok_or(Error::NoSuchChange(id))?;
        let mut out = Vec::new();
        loop {
            let snap = g.snapshots.get(&cur.0).ok_or(Error::NoSuchSnapshot)?;
            out.push(snap.clone());
            match snap.predecessors.first() {
                Some(p) => cur = *p,
                None => break,
            }
        }
        Ok(out)
    }

    fn put_changeset(&self, cs: &ChangeSet) -> Result<()> {
        self.lock()?.changesets.insert(cs.id, cs.clone());
        Ok(())
    }

    fn get_changeset(&self, id: ChangeSetId) -> Result<ChangeSet> {
        self.lock()?
            .changesets
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn changesets(&self) -> Result<Vec<ChangeSet>> {
        Ok(self.lock()?.changesets.values().cloned().collect())
    }

    fn set_branch(&self, name: &str, id: ChangeId) -> Result<()> {
        self.lock()?.branches.insert(name.to_string(), id);
        Ok(())
    }

    fn branch(&self, name: &str) -> Result<Option<ChangeId>> {
        Ok(self.lock()?.branches.get(name).copied())
    }

    fn branches(&self) -> Result<Vec<(String, ChangeId)>> {
        Ok(self
            .lock()?
            .branches
            .iter()
            .map(|(n, id)| (n.clone(), *id))
            .collect())
    }

    fn resolve_prefix(&self, prefix: &str) -> Result<ChangeId> {
        let g = self.lock()?;
        let p = prefix.to_ascii_lowercase();
        let mut hits: Vec<ChangeId> = g
            .heads
            .keys()
            .copied()
            .filter(|id| id.short().starts_with(&p) || id.to_string().replace('-', "").contains(&p))
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
        self.lock()?.root.ok_or(Error::NoSuchSnapshot)
    }

    fn set_root(&self, id: SnapshotId) -> Result<()> {
        self.lock()?.root = Some(id);
        Ok(())
    }

    fn open_changeset(&self) -> Result<Option<OpenChangeSet>> {
        Ok(self.lock()?.open_changeset.clone())
    }

    fn set_open_changeset(&self, row: Option<OpenChangeSet>) -> Result<()> {
        self.lock()?.open_changeset = row;
        Ok(())
    }

    fn render_pending(&self) -> Result<bool> {
        Ok(self.lock()?.render_pending)
    }

    fn set_render_pending(&self, v: bool) -> Result<()> {
        self.lock()?.render_pending = v;
        Ok(())
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
