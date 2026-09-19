use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub Uuid);

        /// Compact bytes for postcard (the hashed form); hyphenated text for JSON.
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                if s.is_human_readable() {
                    s.serialize_str(&self.0.hyphenated().to_string())
                } else {
                    uuid::serde::compact::serialize(&self.0, s)
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                if d.is_human_readable() {
                    let s = String::deserialize(d)?;
                    Uuid::parse_str(&s).map(Self).map_err(serde::de::Error::custom)
                } else {
                    uuid::serde::compact::deserialize(d).map(Self)
                }
            }
        }

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }

            /// Short form from blake3 of the id, never the leading timestamp bits of a v7.
            pub fn short(self) -> String {
                hex4(&self.0.as_bytes()[..])
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

macro_rules! hash_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub [u8; 32]);

        /// Raw bytes for postcard (the hashed form); 64 hex chars for JSON.
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                if s.is_human_readable() {
                    s.serialize_str(&hex32(&self.0))
                } else {
                    self.0.serialize(s)
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                if d.is_human_readable() {
                    let s = String::deserialize(d)?;
                    parse_hex32(&s).map(Self).map_err(serde::de::Error::custom)
                } else {
                    <[u8; 32]>::deserialize(d).map(Self)
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> std::result::Result<Self, String> {
                parse_hex32(s).map(Self)
            }
        }

        impl $name {
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            pub fn short(self) -> String {
                hex4(&self.0)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), hex32(&self.0))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&hex32(&self.0))
            }
        }
    };
}

uuid_id!(EntityId);
uuid_id!(ChangeId);
uuid_id!(ChangeSetId);

impl EntityId {
    /// Declaration-site sentinel. A real self-id makes rename detection circular.
    pub const SELF: Self = Self(Uuid::nil());
}

hash_id!(SnapshotId);
hash_id!(ContentId);
hash_id!(BytesId);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct OpIx(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct Slot(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct TokenIx(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct AtomIx(pub u32);

impl AtomIx {
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct ByteRange {
    pub start: u32,
    pub end: u32,
}

impl ByteRange {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn contains(self, off: u32) -> bool {
        off >= self.start && off < self.end
    }
}

pub type Timestamp = u64;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct LineCol {
    /// 1-based.
    pub line: u32,
    /// 0-based.
    pub col: u32,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RelPath(String);

impl RelPath {
    pub fn new(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if s.is_empty() || s.starts_with('/') || s.contains('\0') || s.contains('\\') {
            return Err(s);
        }
        if s.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return Err(s);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn extension(&self) -> Option<&str> {
        std::path::Path::new(&self.0)
            .extension()
            .and_then(|e| e.to_str())
    }
}

impl std::fmt::Debug for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RelPath({:?})", self.0)
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct ToolCallId(pub String);

pub fn hash_postcard(value: &impl Serialize) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    postcard::to_io(value, &mut hasher).expect("postcard to hasher");
    *hasher.finalize().as_bytes()
}

fn hex4(bytes: &[u8]) -> String {
    let h = blake3::hash(bytes);
    hex_n(h.as_bytes(), 2)
}

fn hex32(bytes: &[u8; 32]) -> String {
    hex_n(bytes, 32)
}

fn hex_n(bytes: &[u8], n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for b in bytes.iter().take(n) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl SnapshotId {
    pub fn of(snapshot: &impl Serialize) -> Self {
        Self(hash_postcard(snapshot))
    }
}

impl ContentId {
    pub fn of(content: &impl Serialize) -> Self {
        Self(hash_postcard(content))
    }
}

impl BytesId {
    pub fn of(bytes: &impl Serialize) -> Self {
        Self(hash_postcard(bytes))
    }
}

fn parse_hex32(s: &str) -> std::result::Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
        out[i] = u8::from_str_radix(pair, 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_compact_in_postcard_and_text_in_json() {
        let c = ChangeId::new();
        assert_eq!(postcard::to_stdvec(&c).unwrap().len(), 16);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, format!("\"{}\"", c.0.hyphenated()));
        assert_eq!(serde_json::from_str::<ChangeId>(&json).unwrap(), c);
        assert_eq!(postcard::from_bytes::<ChangeId>(&postcard::to_stdvec(&c).unwrap()).unwrap(), c);

        let h = SnapshotId([7; 32]);
        assert_eq!(postcard::to_stdvec(&h).unwrap().len(), 32);
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json.len(), 66);
        assert_eq!(serde_json::from_str::<SnapshotId>(&json).unwrap(), h);
        assert_eq!(json.trim_matches('"').parse::<SnapshotId>().unwrap(), h);
    }
}
