use std::collections::BTreeMap;

use crate::content::{Bytes, Content, IdentRef};
use crate::delta::{Delta, ObservedClass};
use crate::entity::{EntityRecord, FileRecord};
use crate::error::{Error, Result};
use crate::ids::{ByteRange, BytesId, ChangeId, ContentId, EntityId, RelPath, SnapshotId};
use crate::lang::{Env, Lang, Langs, RawEntity, Resolution};
use crate::snapshot::Snapshot;
use crate::store::Store;

mod bytes;
mod canon;
mod extract;
mod render;

#[derive(Clone, Debug, Default)]
pub struct Rendered {
    pub files: BTreeMap<RelPath, Vec<u8>>,
    /// Ranges index the rendered entity buffer. Absent when `with_maps` is false.
    pub maps: Option<BTreeMap<EntityId, Vec<(ByteRange, IdentRef)>>>,
}

pub fn parse(src: &[u8], lang: &dyn Lang) -> Result<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&lang.language())
        .map_err(|e| Error::Parse(e.to_string()))?;
    parser
        .parse(src, None)
        .ok_or_else(|| Error::Parse("tree-sitter returned None".into()))
}

pub fn extract(
    tree: &tree_sitter::Tree,
    src: &[u8],
    lang: &dyn Lang,
) -> Result<Vec<RawEntity>> {
    extract::extract(tree, src, lang)
}

pub fn env_at(
    store: &dyn Store,
    snapshot: &Snapshot,
    parent: Option<EntityId>,
) -> Result<Env> {
    let _ = (store, snapshot, parent);
    todo!("env_at")
}

pub fn resolve(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    _env: &Env,
) -> Result<Resolution> {
    Ok(canon::resolve_locals(item, src, lang))
}

pub fn to_bytes(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    resolution: &Resolution,
    children: &[(ByteRange, EntityId)],
    own_name: Option<EntityId>,
) -> Result<Bytes> {
    let extent = extract::byte_range(item);
    let name = own_name.and_then(|id| {
        item.child_by_field_name("name")
            .map(|n| (extract::byte_range(n), id))
    });
    bytes::bytes_from_span(src, extent, children, name, resolution)
}

pub fn canonicalize(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    res: &Resolution,
    children: &[(ByteRange, EntityId)],
    _env: &Env,
    lang: &dyn Lang,
) -> Result<Content> {
    canon::canonicalize(item, src, res, children, lang)
}

pub fn classify(
    old: &Content,
    new: &Content,
    old_bytes: BytesId,
    new_bytes: BytesId,
    old_render: &[u8],
    new_render: &[u8],
    old_res: &Resolution,
    new_res: &Resolution,
    old_map: &[(ByteRange, IdentRef)],
    new_map: &[(ByteRange, IdentRef)],
) -> ObservedClass {
    let _ = (
        old, new, old_bytes, new_bytes, old_render, new_render, old_res, new_res, old_map, new_map,
    );
    todo!("classify")
}

pub fn match_entities(
    prev: &Snapshot,
    parsed: &[RawEntity],
    hashes: &[(ContentId, BytesId)],
) -> Vec<(usize, Option<EntityId>)> {
    let _ = (prev, parsed, hashes);
    todo!("match_entities")
}

pub fn render(
    snapshot: &Snapshot,
    store: &dyn Store,
    _langs: &Langs,
    with_maps: bool,
) -> Result<Rendered> {
    render::render(snapshot, store, with_maps)
}

pub fn render_entity(
    snapshot: &Snapshot,
    store: &dyn Store,
    id: EntityId,
    with_map: bool,
) -> Result<(Vec<u8>, Option<Vec<(ByteRange, IdentRef)>>)> {
    render::render_entity(snapshot, store, id, with_map)
}

pub fn diff(prev: &Snapshot, next: &Snapshot) -> Vec<Delta> {
    let _ = (prev, next);
    todo!("diff")
}

pub fn merge(
    store: &dyn Store,
    langs: &Langs,
    base: SnapshotId,
    a: SnapshotId,
    b: SnapshotId,
) -> Result<Snapshot> {
    let _ = (store, langs, base, a, b);
    todo!("merge")
}

/// Parse one file into a snapshot. Content hashes are empty until canonicalize lands.
pub fn ingest_file(
    src: &[u8],
    path: RelPath,
    lang: &dyn Lang,
    store: &dyn Store,
    change: ChangeId,
) -> Result<Snapshot> {
    let tree = parse(src, lang)?;
    let raw = extract(&tree, src, lang)?;
    let ids: Vec<EntityId> = (0..raw.len()).map(|_| EntityId::new()).collect();
    let empty = Content::default();
    let empty_id = store.put_content(&empty)?;

    let mut entities = BTreeMap::new();
    for (i, ent) in raw.iter().enumerate() {
        let children: Vec<(ByteRange, EntityId)> = ent
            .children
            .iter()
            .map(|&c| (raw[c].bytes_range, ids[c]))
            .collect();
        let own_name = ent.name_range.map(|r| (r, EntityId::SELF));
        let bytes = bytes::bytes_from_span(
            src,
            ent.bytes_range,
            &children,
            own_name,
            &Resolution::default(),
        )?;
        let bytes_id = store.put_bytes_blob(&bytes)?;
        let ordinal = raw
            .iter()
            .enumerate()
            .filter(|(_, o)| o.parent_idx == ent.parent_idx && o.item_range.start < ent.item_range.start)
            .count() as u32;
        entities.insert(
            ids[i],
            EntityRecord {
                name: ent.name.clone(),
                kind: ent.kind,
                parent: ent.parent_idx.map(|p| ids[p]),
                file: path.clone(),
                ordinal,
                content: empty_id,
                bytes: bytes_id,
            },
        );
    }

    let roots: Vec<_> = raw.iter().filter(|e| e.parent_idx.is_none()).cloned().collect();
    let trailing = render::trailing_for(src, &roots);
    let mut files = BTreeMap::new();
    files.insert(path, FileRecord { trailing });

    Ok(Snapshot {
        parents: Vec::new(),
        predecessors: Vec::new(),
        change,
        entities,
        files,
        conflicts: Vec::new(),
        message: String::new(),
    })
}
