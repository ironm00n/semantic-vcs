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
    Removed(EntityId),
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
}
