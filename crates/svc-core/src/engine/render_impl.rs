use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::content::{Chunk, IdentRef};
use crate::error::{Error, Result};
use crate::ids::{ByteRange, EntityId, RelPath};
use crate::snapshot::Snapshot;
use crate::store::Store;

use super::Rendered;

pub fn render(snapshot: &Snapshot, store: &dyn Store, with_maps: bool) -> Result<Rendered> {
    let mut files = BTreeMap::new();
    let mut maps = with_maps.then(BTreeMap::new);
    for (path, rec) in &snapshot.files {
        let mut buf = Vec::new();
        for id in snapshot.file_roots(path) {
            let (bytes, map) = expand(snapshot, store, id, with_maps)?;
            buf.extend_from_slice(&bytes);
            if let (Some(maps), Some(map)) = (maps.as_mut(), map) {
                maps.insert(id, map);
            }
        }
        buf.extend_from_slice(&rec.tail(store)?);
        files.insert(path.clone(), buf);
    }
    Ok(Rendered { files, maps })
}

pub fn render_entity(
    snapshot: &Snapshot,
    store: &dyn Store,
    id: EntityId,
    with_map: bool,
) -> Result<(Vec<u8>, Option<Vec<(ByteRange, IdentRef)>>)> {
    expand(snapshot, store, id, with_map)
}

fn expand(
    snapshot: &Snapshot,
    store: &dyn Store,
    id: EntityId,
    with_map: bool,
) -> Result<(Vec<u8>, Option<Vec<(ByteRange, IdentRef)>>)> {
    let rec = snapshot.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
    let bytes = store.get_bytes_blob(rec.bytes)?;
    let mut out = Vec::new();
    let mut map = with_map.then(Vec::new);
    for chunk in bytes.chunks() {
        match chunk {
            Chunk::Literal(r) => {
                let a = r.start as usize;
                let b = r.end as usize;
                let src = bytes.src();
                if b > src.len() || a > b {
                    return Err(Error::Other("literal chunk out of range".into()));
                }
                let base = out.len() as u32;
                out.extend_from_slice(&src[a..b]);
                if let Some(map) = map.as_mut() {
                    for (lr, ident) in bytes.local_ranges() {
                        if lr.start >= r.start && lr.end <= r.end {
                            map.push((
                                ByteRange {
                                    start: base + (lr.start - r.start),
                                    end: base + (lr.end - r.start),
                                },
                                ident.clone(),
                            ));
                        }
                    }
                }
            }
            Chunk::Child(cid) => {
                let (child, child_map) = expand(snapshot, store, *cid, with_map)?;
                out.extend_from_slice(&child);
                if let (Some(map), Some(child_map)) = (map.as_mut(), child_map) {
                    let base = (out.len() - child.len()) as u32;
                    for (r, ident) in child_map {
                        map.push((
                            ByteRange {
                                start: base + r.start,
                                end: base + r.end,
                            },
                            ident,
                        ));
                    }
                }
            }
            Chunk::Name(nid) => {
                let name = if *nid == EntityId::SELF {
                    rec.name.as_str()
                } else {
                    snapshot
                        .entities
                        .get(nid)
                        .map(|e| e.name.as_str())
                        .unwrap_or("?")
                };
                let name = render_ident(&rec.file, name);
                let start = out.len() as u32;
                out.extend_from_slice(name.as_bytes());
                if let Some(map) = map.as_mut() {
                    map.push((
                        ByteRange {
                            start,
                            end: out.len() as u32,
                        },
                        IdentRef::Entity(*nid),
                    ));
                }
            }
            Chunk::ShorthandName { id: nid, field } => {
                let name = if *nid == EntityId::SELF {
                    rec.name.as_str()
                } else {
                    snapshot
                        .entities
                        .get(nid)
                        .map(|e| e.name.as_str())
                        .unwrap_or("?")
                };
                if name == field.as_ref() {
                    let shown = render_ident(&rec.file, name);
                    let start = out.len() as u32;
                    out.extend_from_slice(shown.as_bytes());
                    if let Some(map) = map.as_mut() {
                        map.push((
                            ByteRange {
                                start,
                                end: out.len() as u32,
                            },
                            IdentRef::Entity(*nid),
                        ));
                    }
                } else {
                    out.extend_from_slice(field.as_bytes());
                    out.extend_from_slice(b": ");
                    let shown = render_ident(&rec.file, name);
                    let start = out.len() as u32;
                    out.extend_from_slice(shown.as_bytes());
                    if let Some(map) = map.as_mut() {
                        map.push((
                            ByteRange {
                                start,
                                end: out.len() as u32,
                            },
                            IdentRef::Entity(*nid),
                        ));
                    }
                }
            }
        }
    }
    Ok((out, map))
}

fn render_ident<'a>(file: &RelPath, name: &'a str) -> Cow<'a, str> {
    if !file.as_str().ends_with(".rs") || name.starts_with("r#") || !is_rust_keyword(name) {
        return Cow::Borrowed(name);
    }
    Cow::Owned(format!("r#{name}"))
}

fn is_rust_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "gen"
            | "macro"
            | "override"
            | "priv"
            | "try"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
    )
}

pub fn trailing_for(src: &[u8], roots: &[crate::lang::RawEntity]) -> Vec<u8> {
    let end = roots.iter().map(|e| e.bytes_range.end).max().unwrap_or(0) as usize;
    src.get(end.min(src.len())..).unwrap_or(&[]).to_vec()
}
