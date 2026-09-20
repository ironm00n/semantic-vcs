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
        /// Recorded target. Absent on ops from before this field existed
        /// (JSON) and on tests that still omit it; apply then uses the parent
        /// file, else the first tracked source file.
        #[serde(default)]
        file: Option<RelPath>,
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
    /// Resolution of conflict `conflict` (an index into the merge snapshot's list) by
    /// taking one side. Recorded as itself so replay reproduces the resolved snapshot.
    Resolve {
        conflict: u32,
        take: Take,
    },
    /// `svc op restore <n>`: return to the view as it stood right after op `n`.
    /// Distinct from `Undo` so the log names the verb.
    ///
    /// Last on purpose: postcard writes a variant as its index, so a variant added
    /// anywhere but the end re-labels every op already in every store (a `New` read
    /// back as `Restore`). `op_wire_format` pins the order.
    Restore {
        at: u64,
    },
}

/// Which side a conflict resolution keeps: the merge's first parent, its second, or the base.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Take {
    A,
    B,
    Base,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::EntityId;

    #[test]
    fn add_def_json_without_file_defaults_to_none() {
        let id = EntityId::new();
        let v = serde_json::json!({
            "AddDef": {
                "id": id,
                "parent": null,
                "ordinal": 0,
                "definition": "fn x() {}",
                "intent": "Feature"
            }
        });
        let op: Op = serde_json::from_value(v).unwrap();
        match op {
            Op::AddDef {
                file, definition, ..
            } => {
                assert!(file.is_none());
                assert_eq!(definition, "fn x() {}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn restore_json_round_trips_the_op_index() {
        let op = Op::Restore { at: 3 };
        let v = serde_json::to_value(&op).unwrap();
        assert_eq!(v, serde_json::json!({"Restore":{"at":3}}));
        let back: Op = serde_json::from_value(v).unwrap();
        assert!(matches!(back, Op::Restore { at: 3 }));
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
