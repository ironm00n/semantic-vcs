//! The repository layer over `svc-core`: a redb-backed [`Store`](svc_core::Store), the
//! checked-out [`Repo`] with its snapshot-before/render-after mutation wrapper, and the
//! history verbs (`new`, `describe`, `branch`, `evolog`, `log`, `undo`, …) as library
//! functions returning serialisable records. The CLI is a thin layer over these.

pub mod history;
pub mod merge;
pub mod repo;
pub mod store;

pub use history::*;
pub use merge::{MergeOut, Take, conflicts, lca, merge, merge_snapshots, resolve};
pub use repo::{Mutation, Repo};
pub use store::RedbStore;
