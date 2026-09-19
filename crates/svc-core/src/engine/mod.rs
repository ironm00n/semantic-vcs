use std::collections::BTreeMap;

use crate::content::{Bytes, Content, IdentRef};
use crate::delta::{Delta, ObservedClass};
use crate::error::Result;
use crate::ids::{ByteRange, BytesId, ContentId, EntityId, RelPath, SnapshotId};
use crate::lang::{Env, Lang, Langs, RawEntity, Resolution};
use crate::snapshot::Snapshot;
use crate::store::Store;

#[derive(Clone, Debug, Default)]
pub struct Rendered {
    pub files: BTreeMap<RelPath, Vec<u8>>,
    /// Ranges index the rendered entity buffer. Absent when `with_maps` is false.
    pub maps: Option<BTreeMap<EntityId, Vec<(ByteRange, IdentRef)>>>,
}

pub fn extract(
    tree: &tree_sitter::Tree,
    src: &[u8],
    lang: &dyn Lang,
) -> Result<Vec<RawEntity>> {
    let _ = (tree, src, lang);
    todo!("extract")
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
    env: &Env,
) -> Result<Resolution> {
    let _ = (item, src, lang, env);
    todo!("resolve")
}

pub fn to_bytes(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    resolution: &Resolution,
    children: &[(ByteRange, EntityId)],
    own_name: Option<EntityId>,
) -> Result<Bytes> {
    let _ = (item, src, resolution, children, own_name);
    todo!("to_bytes")
}

pub fn canonicalize(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    res: &Resolution,
    children: &[(ByteRange, EntityId)],
    env: &Env,
) -> Result<Content> {
    let _ = (item, src, res, children, env);
    todo!("canonicalize")
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
    langs: &Langs,
    with_maps: bool,
) -> Result<Rendered> {
    let _ = (snapshot, store, langs, with_maps);
    todo!("render")
}

pub fn render_entity(
    snapshot: &Snapshot,
    store: &dyn Store,
    id: EntityId,
    with_map: bool,
) -> Result<(Vec<u8>, Option<Vec<(ByteRange, IdentRef)>>)> {
    let _ = (snapshot, store, id, with_map);
    todo!("render_entity")
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
