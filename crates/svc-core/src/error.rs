use crate::ids::{ChangeId, EntityId, RelPath};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("backend: {0}")]
    Backend(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("no such snapshot")]
    NoSuchSnapshot,
    #[error("no such entity {0}")]
    NoSuchEntity(EntityId),
    #[error("no such change {0}")]
    NoSuchChange(ChangeId),
    #[error("ambiguous prefix {prefix:?}: {candidates:?}")]
    AmbiguousPrefix {
        prefix: String,
        candidates: Vec<ChangeId>,
    },
    #[error("parse error: {0}")]
    Parse(String),
    #[error("conflicted")]
    Conflicted,
    #[error("invalid path {0:?}")]
    InvalidPath(String),
    #[error("merge arity: adds must be removes + 1 (adds={adds}, removes={removes})")]
    MergeArity { adds: usize, removes: usize },
    #[error("bytes encoding is not canonical: {0}")]
    BytesCanonical(String),
    #[error("no language for {0}")]
    NoLanguage(RelPath),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn backend<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Self::Backend(Box::new(e))
    }
}
