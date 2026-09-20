//! The repository layer over `svc-core`: a redb-backed [`Store`](svc_core::Store), the
//! checked-out [`Repo`] with its snapshot-before/render-after mutation wrapper, and the
//! history verbs (`new`, `describe`, `branch`, `evolog`, `log`, `undo`, …) as library
//! functions returning serialisable records. The CLI is a thin layer over these.

pub mod forge;
pub mod history;
pub mod merge;
pub mod repo;
pub mod replay;
pub mod store;
pub mod text;
pub mod workspace;

pub use history::*;
pub use replay::{ReplayReport, replay};
pub use merge::{ConflictOut, MergeOut, Take, conflicts, lca, merge, merged_snapshot, resolve, resolved_snapshot};
pub use repo::{Mutation, Repo};
pub use store::RedbStore;
pub use workspace::{WorkspaceOut, WorkspacePointer};
pub use store::WorkspaceRow;
