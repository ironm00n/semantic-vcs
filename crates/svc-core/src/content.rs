use serde::{Deserialize, Serialize};

use crate::ids::{ByteRange, BytesId, ContentId, EntityId, Slot};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub enum Namespace {
    Value,
    Type,
    Macro,
    Lifetime,
    Label,
    /// `use` aliases have no namespace; lookup matches any.
    Any,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub enum IdentRef {
    Local(Slot, Namespace),
    Entity(EntityId),
    Free(Box<str>),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub enum Token {
    Punct(Box<str>),
    Kw(Box<str>),
    Lit(Box<str>),
    Binder(Slot, Namespace),
    Ident(IdentRef),
    Child(EntityId),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct Content {
    pub tokens: Vec<Token>,
}

impl Content {
    pub fn id(&self) -> ContentId {
        ContentId::of(self)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Chunk {
    Literal(ByteRange),
    Child(EntityId),
    /// Entity-reference hole. `EntityId::SELF` is this entity's own declaration site.
    Name(EntityId),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Bytes {
    src: Vec<u8>,
    chunks: Vec<Chunk>,
    local_ranges: Vec<(ByteRange, IdentRef)>,
}

impl Bytes {
    /// Enforces: no adjacent `Literal` chunks; `local_ranges` sorted by start.
    pub fn new(
        src: Vec<u8>,
        chunks: Vec<Chunk>,
        mut local_ranges: Vec<(ByteRange, IdentRef)>,
    ) -> crate::error::Result<Self> {
        let chunks = coalesce_literals(chunks);
        local_ranges.sort_by_key(|(r, _)| r.start);
        for w in local_ranges.windows(2) {
            if w[0].0.start == w[1].0.start {
                return Err(crate::error::Error::BytesCanonical(
                    "local_ranges not unique by start".into(),
                ));
            }
        }
        Ok(Self {
            src,
            chunks,
            local_ranges,
        })
    }

    pub fn src(&self) -> &[u8] {
        &self.src
    }

    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    pub fn local_ranges(&self) -> &[(ByteRange, IdentRef)] {
        &self.local_ranges
    }

    pub fn id(&self) -> BytesId {
        BytesId::of(self)
    }
}

fn coalesce_literals(chunks: Vec<Chunk>) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::with_capacity(chunks.len());
    for ch in chunks {
        match (out.last_mut(), ch) {
            (Some(Chunk::Literal(prev)), Chunk::Literal(next)) if prev.end == next.start => {
                prev.end = next.end;
            }
            (_, ch) => out.push(ch),
        }
    }
    out
}
