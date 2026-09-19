use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::delta::ObservedClass;
use crate::ids::{ChangeId, ChangeSetId, EntityId, RelPath, SnapshotId, Timestamp};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Intent {
    Refactor,
    Fix,
    Feature,
    Docs,
    Other(String),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Op {
    Rename {
        id: EntityId,
        new: String,
    },
    Move {
        id: EntityId,
        parent: Option<EntityId>,
        ordinal: Option<u32>,
    },
    Relocate {
        id: EntityId,
        file: RelPath,
        ordinal: u32,
    },
    Extract {
        id: EntityId,
        new_parent: Option<EntityId>,
        ordinal: u32,
    },
    Inline {
        id: EntityId,
    },
    AddDef {
        id: EntityId,
        parent: Option<EntityId>,
        ordinal: u32,
        definition: String,
        intent: Intent,
    },
    Delete {
        id: EntityId,
        intent: Intent,
    },
    EditDef {
        id: EntityId,
        definition: String,
        intent: Intent,
    },
    Merge {
        other: ChangeId,
    },
    Undo,
    New {
        change: ChangeId,
    },
    Describe {
        msg: String,
    },
    /// Start a sibling change: parent is the current change's parent, then name it.
    Branch {
        name: String,
    },
    /// Working-copy reconciliation. Snapshot is `after.root` on the log entry so O5 can replay.
    Absorb,
}

impl Op {
    pub fn intent(&self) -> Option<&Intent> {
        match self {
            Op::AddDef { intent, .. } | Op::EditDef { intent, .. } | Op::Delete { intent, .. } => {
                Some(intent)
            }
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct View {
    pub root: SnapshotId,
    pub heads: BTreeMap<ChangeId, SnapshotId>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct OpLogEntry {
    pub op: Op,
    pub observed: Option<ObservedClass>,
    pub at: Timestamp,
    pub group: Option<ChangeSetId>,
    pub before: View,
    pub after: View,
}

impl OpLogEntry {
    pub fn declared(&self) -> Option<&Intent> {
        self.op.intent()
    }

    pub fn flagged(&self) -> bool {
        match (self.declared(), self.observed) {
            (Some(Intent::Refactor), Some(ObservedClass::BindingChanging)) => true,
            (Some(Intent::Docs), Some(o))
                if !matches!(o, ObservedClass::DocsOnly | ObservedClass::Alpha) =>
            {
                true
            }
            (Some(Intent::Fix), Some(ObservedClass::Alpha | ObservedClass::DocsOnly)) => true,
            (None, Some(ObservedClass::BindingChanging)) => true,
            _ => false,
        }
    }
}
