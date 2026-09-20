use serde::{Deserialize, Serialize};

use crate::ids::{EntityId, RelPath};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ObservedClass {
    Alpha,
    DocsOnly,
    BindingPreserving,
    BindingChanging,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Delta {
    Added(EntityId),
    /// Carries the name because the entity is gone from the snapshot the reader has.
    Removed {
        id: EntityId,
        name: String,
    },
    Renamed {
        id: EntityId,
        from: String,
        to: String,
    },
    Moved {
        id: EntityId,
        from_parent: Option<EntityId>,
        to_parent: Option<EntityId>,
    },
    Relocated {
        id: EntityId,
        from: (RelPath, u32),
        to: (RelPath, u32),
    },
    Edited(EntityId, ObservedClass),
    /// Bytes are identical; the canonical form moved (engine drift, or a re-bind
    /// caused by another entity). Not an edit: the working copy did not change.
    Rebound(EntityId, ObservedClass),
    /// A file with no language, or a source file's bytes outside every entity.
    FileAdded(RelPath),
    FileRemoved(RelPath),
    /// Bytes after the last entity changed: an opaque file's whole content, or a
    /// source file's tail. `whitespace_only` when the two tails differ in nothing else.
    FileTail {
        path: RelPath,
        whitespace_only: bool,
    },
}
