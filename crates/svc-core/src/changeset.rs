use serde::{Deserialize, Serialize};

use crate::delta::ObservedClass;
use crate::ids::{ChangeSetId, OpIx, Timestamp, ToolCallId};
use crate::op::Intent;
use crate::snapshot::Conflict;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ReviewItem {
    EditReview {
        op: OpIx,
        declared: Intent,
        observed: Option<ObservedClass>,
        ask_id: Option<ToolCallId>,
    },
    BindingConflict {
        conflict: Conflict,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ChangeSet {
    pub id: ChangeSetId,
    pub name: String,
    pub intent: Intent,
    pub queue: Vec<ReviewItem>,
    pub description: String,
}

/// Process-global open group. `begin` records `pid: None` and a TTL; only long-lived
/// openers (`svc agent`) record a pid.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct OpenChangeSet {
    pub id: ChangeSetId,
    pub pid: Option<u32>,
    pub opened_at: Timestamp,
}

pub const CHANGESET_TTL_MS: u64 = 30 * 60 * 1000;
