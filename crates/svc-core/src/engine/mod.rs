use std::collections::BTreeMap;

use crate::content::{Bytes, Content, IdentRef, Namespace};
use crate::delta::ObservedClass;
use crate::entity::{EntityRecord, FileRecord};
use crate::error::{Error, Result};
use crate::ids::{ByteRange, BytesId, ChangeId, ContentId, EntityId, RelPath};
use crate::lang::{Env, Lang, Langs, RawEntity, Resolution};
use crate::snapshot::Snapshot;
use crate::store::Store;

mod align;
mod bytes;
mod canon;
mod classify;
mod diff_impl;
mod extract;
mod merge;
mod ops;
mod render_impl;

pub use classify::classify;
pub use merge::{lca, merge};
pub use ops::{
    add_def, classify_def, commit_snapshot, delete, edit_def, extract_hoist, format_tokens, inline,
    lookup, lookup_name, move_def, redefine, relocate, rename, rust_langs, show,
    snapshot_working_copy, status_report, StatusReport,
};

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
    _store: &dyn Store,
    snapshot: &Snapshot,
    _parent: Option<EntityId>,
) -> Result<Env> {
    Ok(env_from_snapshot(snapshot))
}

/// Name + refined Kind for Muse's JS lane tests (nested `let`s stay locals).
pub fn js_extract_refined_kinds(src: &str) -> Result<Vec<(String, crate::entity::Kind)>> {
    let lang = crate::JsLang;
    let tree = parse(src.as_bytes(), &lang)?;
    Ok(extract(&tree, src.as_bytes(), &lang)?
        .into_iter()
        .map(|e| (e.name, e.kind))
        .collect())
}

pub fn env_from_snapshot(snapshot: &Snapshot) -> Env {
    let mut env = Env::default();
    for (id, rec) in &snapshot.entities {
        env.insert(&rec.name, Namespace::Value, *id);
        env.insert(&rec.name, Namespace::Type, *id);
    }
    env
}

pub fn resolve(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    env: &Env,
) -> Result<Resolution> {
    let mut res = canon::resolve_locals(item, src, lang);
    for (_, ident) in &mut res.refs {
        if let IdentRef::Free(n) = ident {
            if let Some(id) = env
                .lookup(n, Namespace::Value)
                .or_else(|| env.lookup(n, Namespace::Type))
            {
                *ident = IdentRef::Entity(id);
            }
        }
    }
    Ok(res)
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

pub fn match_entities(
    prev: &Snapshot,
    parsed: &[RawEntity],
    hashes: &[(ContentId, BytesId)],
) -> Vec<(usize, Option<EntityId>)> {
    diff_impl::match_entities(prev, parsed, hashes)
}

pub fn render(
    snapshot: &Snapshot,
    store: &dyn Store,
    _langs: &Langs,
    with_maps: bool,
) -> Result<Rendered> {
    render_impl::render(snapshot, store, with_maps)
}

pub fn render_entity(
    snapshot: &Snapshot,
    store: &dyn Store,
    id: EntityId,
    with_map: bool,
) -> Result<(Vec<u8>, Option<Vec<(ByteRange, IdentRef)>>)> {
    render_impl::render_entity(snapshot, store, id, with_map)
}

pub fn diff(prev: &Snapshot, next: &Snapshot) -> Vec<crate::delta::Delta> {
    diff_impl::diff(prev, next)
}

/// Bytes-in → snapshot-out. Does not write the snapshot, set root/heads, or append an op.
pub fn snapshot_files(
    store: &dyn Store,
    langs: &Langs,
    files: &BTreeMap<RelPath, Vec<u8>>,
    prev: Option<&Snapshot>,
    change: ChangeId,
) -> Result<Snapshot> {
    struct Parsed<'a> {
        path: RelPath,
        src: &'a [u8],
        tree: tree_sitter::Tree,
        raw: Vec<RawEntity>,
        lang: &'a dyn Lang,
        ids: Vec<EntityId>,
    }
    let mut parsed = Vec::new();
    let mut opaque = BTreeMap::new();
    for (path, src) in files {
        match langs.for_path(path) {
            Some(lang) => {
                let tree = parse(src, lang)?;
                let raw = extract(&tree, src, lang)?;
                let ids = assign_ids(&raw, prev);
                parsed.push(Parsed {
                    path: path.clone(),
                    src,
                    tree,
                    raw,
                    lang,
                    ids,
                });
            }
            // Manifests, lockfiles, recordings: stored as the file tail with no
            // entities so `svc init` on this repo still renders a tree cargo can build.
            None => {
                opaque.insert(path.clone(), src.clone());
            }
        }
    }
    let mut env = prev.map(env_from_snapshot).unwrap_or_default();
    for p in &parsed {
        for (i, ent) in p.raw.iter().enumerate() {
            env.insert(&ent.name, Namespace::Value, p.ids[i]);
            env.insert(&ent.name, Namespace::Type, p.ids[i]);
        }
    }
    let mut entities = BTreeMap::new();
    let mut file_recs = BTreeMap::new();
    for p in &parsed {
        let (ents, file) = materialize(p.src, p.path.clone(), p.lang, store, &p.tree, &p.raw, &p.ids, &env)?;
        entities.extend(ents);
        file_recs.insert(p.path.clone(), file);
    }
    for (path, src) in opaque {
        file_recs.insert(path, FileRecord { trailing: src });
    }
    Ok(Snapshot {
        parents: Vec::new(),
        predecessors: Vec::new(),
        change,
        entities,
        files: file_recs,
        conflicts: Vec::new(),
        message: String::new(),
    })
}

pub fn ingest_file(
    src: &[u8],
    path: RelPath,
    lang: &dyn Lang,
    store: &dyn Store,
    change: ChangeId,
) -> Result<Snapshot> {
    ingest_file_with_env(src, path, lang, store, change, &Env::default())
}

pub fn ingest_file_with_env(
    src: &[u8],
    path: RelPath,
    lang: &dyn Lang,
    store: &dyn Store,
    change: ChangeId,
    extra: &Env,
) -> Result<Snapshot> {
    ingest_file_prev(src, path, lang, store, change, None, extra)
}

pub fn ingest_file_prev(
    src: &[u8],
    path: RelPath,
    lang: &dyn Lang,
    store: &dyn Store,
    change: ChangeId,
    prev: Option<&Snapshot>,
    extra: &Env,
) -> Result<Snapshot> {
    let tree = parse(src, lang)?;
    let raw = extract(&tree, src, lang)?;
    let ids = assign_ids(&raw, prev);
    let mut env = extra.clone();
    for (i, ent) in raw.iter().enumerate() {
        env.insert(&ent.name, Namespace::Value, ids[i]);
        env.insert(&ent.name, Namespace::Type, ids[i]);
    }
    let (entities, file) = materialize(src, path.clone(), lang, store, &tree, &raw, &ids, &env)?;
    let mut files = BTreeMap::new();
    files.insert(path, file);
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

fn materialize(
    src: &[u8],
    path: RelPath,
    lang: &dyn Lang,
    store: &dyn Store,
    tree: &tree_sitter::Tree,
    raw: &[RawEntity],
    ids: &[EntityId],
    env: &Env,
) -> Result<(BTreeMap<EntityId, EntityRecord>, FileRecord)> {
    let mut entities = BTreeMap::new();
    for (i, ent) in raw.iter().enumerate() {
        let node = extract::find_node(tree.root_node(), ent.item_range)
            .ok_or_else(|| Error::Parse(format!("no node for {}", ent.name)))?;
        let res = resolve(node, src, lang, env)?;
        let children: Vec<(ByteRange, EntityId)> = ent
            .children
            .iter()
            .map(|&c| (raw[c].bytes_range, ids[c]))
            .collect();
        let own_name = ent.name_range.map(|r| (r, EntityId::SELF));
        let bytes = bytes::bytes_from_span(src, ent.bytes_range, &children, own_name, &res)?;
        let bytes_id = store.put_bytes_blob(&bytes)?;
        let child_spans: Vec<(ByteRange, EntityId)> = ent
            .children
            .iter()
            .map(|&c| (raw[c].item_range, ids[c]))
            .collect();
        let content = canonicalize(node, src, &res, &child_spans, env, lang)?;
        let content_id = store.put_content(&content)?;
        let ordinal = raw
            .iter()
            .filter(|o| o.parent_idx == ent.parent_idx && o.item_range.start < ent.item_range.start)
            .count() as u32;
        entities.insert(
            ids[i],
            EntityRecord {
                name: ent.name.clone(),
                kind: ent.kind,
                parent: ent.parent_idx.map(|p| ids[p]),
                file: path.clone(),
                ordinal,
                content: content_id,
                bytes: bytes_id,
            },
        );
    }
    let roots: Vec<_> = raw
        .iter()
        .filter(|e| e.parent_idx.is_none())
        .cloned()
        .collect();
    let trailing = render_impl::trailing_for(src, &roots);
    Ok((entities, FileRecord { trailing }))
}

fn assign_ids(raw: &[RawEntity], prev: Option<&Snapshot>) -> Vec<EntityId> {
    let mut ids = Vec::with_capacity(raw.len());
    let mut used = BTreeMap::new();
    for ent in raw {
        let parent = ent.parent_idx.map(|p| ids[p]);
        let reuse = prev.and_then(|p| {
            p.entities.iter().find_map(|(id, rec)| {
                if used.contains_key(id) {
                    return None;
                }
                (rec.name == ent.name && rec.kind == ent.kind && rec.parent == parent)
                    .then_some(*id)
            })
        });
        let id = reuse.unwrap_or_else(EntityId::new);
        if let Some(prev) = prev {
            if let Some(rec) = prev.entities.get(&id) {
                used.insert(id, rec.content);
            }
        }
        ids.push(id);
    }
    ids
}

pub fn classify_legacy(
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
    classify(
        old,
        new,
        old_bytes,
        new_bytes,
        old_render,
        new_render,
        old_res,
        new_res,
        old_map,
        new_map,
    )
}
