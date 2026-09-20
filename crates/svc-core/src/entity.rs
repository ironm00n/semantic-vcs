use serde::{Deserialize, Serialize};

use crate::ids::{BytesId, ContentId, EntityId, RelPath};

/// Accessor/static roles are `Kind` variants so `(parent, kind, name)` stays unique
/// without folding `"get "` into the stored name (which would render `get get path()`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub enum Kind {
    Fn,
    Struct,
    Enum,
    Union,
    Trait,
    Impl,
    Const,
    Static,
    Mod,
    TypeAlias,
    Macro,
    JsFunction,
    JsClass,
    JsMethod,
    JsGetter,
    JsSetter,
    JsField,
    JsStaticMethod,
    JsStaticField,
    JsDeclarator,
    JsStaticBlock,
    Opaque,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct SigKey {
    pub parent: Option<EntityId>,
    pub kind: Kind,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct EntityRecord {
    pub name: String,
    pub kind: Kind,
    pub parent: Option<EntityId>,
    pub file: RelPath,
    pub ordinal: u32,
    pub content: ContentId,
    pub bytes: BytesId,
}

/// File tail after the last entity. Roots are derived from `Snapshot.entities`
/// (parent is None, same file, ordinal-sorted) so they cannot drift.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct FileRecord {
    pub trailing: Vec<u8>,
}

impl EntityRecord {
    pub fn sig_key(&self) -> SigKey {
        SigKey {
            parent: self.parent,
            kind: self.kind,
            name: self.name.clone(),
        }
    }
}

impl Kind {
    pub fn is_synthetic_named(self) -> bool {
        matches!(self, Kind::Impl | Kind::JsStaticBlock)
    }
}
