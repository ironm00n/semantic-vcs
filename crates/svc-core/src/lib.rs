//! Semantic store types. Public signatures here are frozen for lane fan-out.

pub mod changeset;
pub mod content;
pub mod delta;
pub mod engine;
pub mod entity;
pub mod error;
pub mod ids;
pub mod lang;
pub mod lang_rust;
pub mod op;
pub mod snapshot;
pub mod store;

pub use changeset::{ChangeSet, OpenChangeSet, ReviewItem, CHANGESET_TTL_MS};
pub use content::{Bytes, Chunk, Content, IdentRef, Namespace, Token};
pub use delta::{Delta, ObservedClass};
pub use engine::Rendered;
pub use entity::{EntityRecord, FileRecord, Kind, SigKey};
pub use error::{Error, Result};
pub use ids::{
    AtomIx, ByteRange, BytesId, ChangeId, ChangeSetId, ContentId, EntityId, LineCol, OpIx, RelPath,
    Slot, SnapshotId, Timestamp, TokenIx, ToolCallId,
};
pub use lang::{
    Barrier, BinderClass, CommutativeRule, EntityKindRule, Env, Lang, Langs, Locator, RawEntity,
    Resolution, Role, Visibility, When,
};
pub use lang_rust::RustLang;
pub use op::{Intent, Op, OpLogEntry, View};
pub use snapshot::{AtomLocal, Conflict, Hunk, Merge, Side, Snapshot};
pub use store::{MemStore, Store};
