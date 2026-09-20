use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::content::IdentRef;
use crate::entity::{EntityRecord, FileRecord, SigKey};
use crate::error::{Error, Result};
use crate::ids::{AtomIx, ChangeId, EntityId, LineCol, RelPath, SnapshotId, TokenIx};

/// Non-empty merge: `adds.len() == removes.len() + 1`. Stored as first add + (remove, add) pairs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Merge<T> {
    head: T,
    tail: Vec<(T, T)>,
}

impl<T> Merge<T> {
    pub fn unit(value: T) -> Self {
        Self {
            head: value,
            tail: Vec::new(),
        }
    }

    pub fn three_way(base: T, a: T, b: T) -> Self {
        Self {
            head: a,
            tail: vec![(base, b)],
        }
    }

    pub fn from_adds_removes(mut adds: Vec<T>, removes: Vec<T>) -> Result<Self> {
        if adds.len() != removes.len() + 1 {
            return Err(Error::MergeArity {
                adds: adds.len(),
                removes: removes.len(),
            });
        }
        let head = adds.remove(0);
        let tail = removes.into_iter().zip(adds).collect();
        Ok(Self { head, tail })
    }

    pub fn adds(&self) -> impl Iterator<Item = &T> {
        std::iter::once(&self.head).chain(self.tail.iter().map(|(_, a)| a))
    }

    pub fn removes(&self) -> impl Iterator<Item = &T> {
        self.tail.iter().map(|(r, _)| r)
    }

    pub fn resolved(&self) -> Option<&T> {
        self.tail.is_empty().then_some(&self.head)
    }

    pub fn is_resolved(&self) -> bool {
        self.tail.is_empty()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum AttrValue {
    Name(String),
    Parent(Option<EntityId>),
    File(RelPath),
    Ordinal(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Side {
    A,
    B,
}

/// `start..end` indexes the **merged** atom sequence, never a per-side stream.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Hunk {
    pub entity: EntityId,
    pub start: AtomIx,
    pub end: AtomIx,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Conflict {
    Attr {
        id: EntityId,
        sides: Merge<AttrValue>,
    },
    Content {
        id: EntityId,
        hunks: Vec<Merge<Hunk>>,
    },
    AddAdd {
        key: SigKey,
        a: EntityId,
        b: EntityId,
    },
    DeleteEdit {
        id: EntityId,
        deleted_by: Side,
        edited_by: Side,
    },
    Binding {
        id: EntityId,
        ident: TokenIx,
        name: String,
        at: LineCol,
        was: IdentRef,
        was_at: Option<LineCol>,
        now: IdentRef,
        now_at: Option<LineCol>,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Snapshot {
    pub parents: Vec<SnapshotId>,
    pub predecessors: Vec<SnapshotId>,
    pub change: ChangeId,
    pub entities: BTreeMap<EntityId, EntityRecord>,
    pub files: BTreeMap<RelPath, FileRecord>,
    pub conflicts: Vec<Conflict>,
    pub message: String,
}

impl Snapshot {
    pub fn id(&self) -> SnapshotId {
        SnapshotId::of(self)
    }

    /// File-level roots: entities in `file` whose parent is missing or lives in another file.
    pub fn file_roots(&self, file: &RelPath) -> Vec<EntityId> {
        let mut roots: Vec<(u32, EntityId)> = self
            .entities
            .iter()
            .filter(|(_, rec)| rec.file == *file)
            .filter(|(_, rec)| match rec.parent {
                None => true,
                Some(p) => self
                    .entities
                    .get(&p)
                    .map(|pr| pr.file != *file)
                    .unwrap_or(true),
            })
            .map(|(id, rec)| (rec.ordinal, *id))
            .collect();
        roots.sort_by_key(|(ord, _)| *ord);
        roots.into_iter().map(|(_, id)| id).collect()
    }

    pub fn content_eq(&self, other: &Self) -> bool {
        self.entities == other.entities
            && self.files == other.files
            && self.conflicts == other.conflicts
            && self.message == other.message
    }

    /// `(parent, kind, name)` is unique except for kinds whose name is synthesised
    /// from position (`impl`, static blocks, opaque `use` lines).
    pub fn insert(&mut self, id: EntityId, rec: EntityRecord) -> Result<()> {
        if !self.files.contains_key(&rec.file) {
            return Err(Error::Other(format!("no file record for {}", rec.file)));
        }
        refuse_duplicate(self, id, &rec.sig_key())?;
        self.entities.insert(id, rec);
        Ok(())
    }

    pub fn rename(&mut self, id: EntityId, new: &str) -> Result<()> {
        let rec = self.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
        refuse_duplicate(
            self,
            id,
            &SigKey {
                parent: rec.parent,
                kind: rec.kind,
                name: new.to_string(),
            },
        )?;
        self.entities.get_mut(&id).unwrap().name = new.to_string();
        Ok(())
    }

    pub fn reparent(
        &mut self,
        id: EntityId,
        parent: Option<EntityId>,
        ordinal: Option<u32>,
    ) -> Result<()> {
        let rec = self.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
        refuse_duplicate(
            self,
            id,
            &SigKey {
                parent,
                kind: rec.kind,
                name: rec.name.clone(),
            },
        )?;
        let rec = self.entities.get_mut(&id).unwrap();
        rec.parent = parent;
        if let Some(o) = ordinal {
            rec.ordinal = o;
        }
        Ok(())
    }

    pub fn set_file(&mut self, id: EntityId, file: RelPath, ordinal: u32) -> Result<()> {
        if !self.files.contains_key(&file) {
            return Err(Error::Other(format!("no file record for {file}")));
        }
        let rec = self.entities.get_mut(&id).ok_or(Error::NoSuchEntity(id))?;
        rec.file = file;
        rec.ordinal = ordinal;
        Ok(())
    }

    pub fn ensure_file(&mut self, file: RelPath) {
        self.files.entry(file).or_default();
    }
}

fn refuse_duplicate(snap: &Snapshot, id: EntityId, key: &SigKey) -> Result<()> {
    use crate::entity::Kind;
    if key.kind.is_synthetic_named() || key.kind == Kind::Opaque {
        return Ok(());
    }
    let clash = snap.entities.iter().find(|(other, r)| {
        **other != id && r.parent == key.parent && r.kind == key.kind && r.name == key.name
    });
    match clash {
        Some((other, _)) => Err(Error::Other(format!(
            "{:?} {} already exists under the same parent ({})",
            key.kind,
            key.name,
            other.short()
        ))),
        None => Ok(()),
    }
}

/// Statement-atom identity for matching is the atom's own hash with locals numbered
/// *within the atom* (`Extern` for out-of-atom binders). `AtomIx` is only an index
/// into one sequence (for `Hunk`) and is never a cross-side key.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum AtomLocal {
    Within(u32),
    Extern,
}
