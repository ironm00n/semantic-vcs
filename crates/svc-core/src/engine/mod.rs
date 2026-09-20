use std::collections::{BTreeMap, HashSet};

use crate::content::{Bytes, Content, IdentRef};
use crate::entity::{EntityRecord, FileRecord, Kind, SigKey};
use crate::error::{Error, Result};
use crate::ids::{ByteRange, ChangeId, EntityId, RelPath};
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

pub use classify::{Side, classify, classify_entity};
pub use merge::{lca, merge};
pub use ops::{
    StatusReport, add_def, add_def_at, classify_def, commit_snapshot, delete, edit_def,
    extract_hoist, format_tokens, inline, lookup, lookup_name, move_def, redefine, relocate,
    rename, resolve_add_def_file, rust_langs, show, status_report,
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

pub fn extract(tree: &tree_sitter::Tree, src: &[u8], lang: &dyn Lang) -> Result<Vec<RawEntity>> {
    extract::extract(tree, src, lang)
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
        if is_inherent_rec(snapshot, rec) {
            continue;
        }
        env.insert_def_in(&rec.name, rec.kind, *id, Some(&rec.file));
    }
    env
}

/// Inherent methods nested under `parent` (an `impl` or class). Empty when the
/// item is file-root: there is no receiver to bind `self.foo()` against.
pub(crate) fn fill_self_methods_from_snapshot(
    env: &mut Env,
    snapshot: &Snapshot,
    parent: Option<EntityId>,
) {
    env.self_methods.clear();
    let Some(parent) = parent else {
        return;
    };
    env.self_methods = snapshot
        .entities
        .iter()
        .filter(|(_, rec)| rec.parent == Some(parent) && is_callable_member(rec.kind))
        .map(|(id, rec)| (rec.name.clone(), *id))
        .collect();
}

fn is_callable_member(kind: Kind) -> bool {
    matches!(kind, Kind::Fn | Kind::JsMethod | Kind::JsStaticMethod)
}

/// Inherent methods live in `self_methods`, not the flat Value env. A free
/// `fn read` and `impl S { fn read(&self) }` are different targets: `read()`
/// is the free fn, `self.read()` is the method.
fn is_inherent_member(kind: Kind, parent_kind: Kind) -> bool {
    match parent_kind {
        Kind::Impl | Kind::Trait => matches!(kind, Kind::Fn),
        Kind::JsClass => matches!(
            kind,
            Kind::JsMethod
                | Kind::JsStaticMethod
                | Kind::JsGetter
                | Kind::JsSetter
                | Kind::JsField
                | Kind::JsStaticField
        ),
        _ => false,
    }
}

fn is_inherent_rec(snapshot: &Snapshot, rec: &EntityRecord) -> bool {
    rec.parent.is_some_and(|p| {
        snapshot
            .entities
            .get(&p)
            .is_some_and(|par| is_inherent_member(rec.kind, par.kind))
    })
}

fn is_inherent_raw(raw: &[RawEntity], i: usize) -> bool {
    raw[i]
        .parent_idx
        .is_some_and(|p| is_inherent_member(raw[i].kind, raw[p].kind))
}

pub fn resolve(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    env: &Env,
) -> Result<Resolution> {
    Ok(canon::resolve_locals(item, src, lang, env))
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

pub fn diff(
    store: &dyn Store,
    prev: &Snapshot,
    next: &Snapshot,
) -> Result<Vec<crate::delta::Delta>> {
    diff_impl::diff(store, prev, next)
}

/// Bytes-in → snapshot-out. Does not write the snapshot, set root/heads, or append an op.
/// `files` is the whole tree; `prev` lends ids only. Names resolve against what is parsed
/// here — never against `prev`, whose entities may be exactly what this edit deleted (a
/// reference kept bound to a gone id renders as `?`).
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
    let mut prev_ids = prev_ids(prev);
    for (path, src) in files {
        match langs.for_path(path) {
            Some(lang) => {
                let tree = parse(src, lang)?;
                let raw = extract(&tree, src, lang)?;
                let ids = assign_ids(&raw, path, &mut prev_ids, prev);
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
    // Prev is for assign_ids. Seeding names from it keeps deleted same-file
    // defs in the env, so a remaining call binds to a missing id and render
    // prints `?` (claude 03:34: absorb after deleting resolve_entity_in).
    let mut env = Env::default();
    for p in &parsed {
        for (i, ent) in p.raw.iter().enumerate() {
            if is_inherent_raw(&p.raw, i) {
                continue;
            }
            env.insert_def_in(&ent.name, ent.kind, p.ids[i], Some(&p.path));
        }
    }
    let mut entities = BTreeMap::new();
    let mut file_recs = BTreeMap::new();
    for p in &parsed {
        let (ents, file) = materialize(
            p.src,
            p.path.clone(),
            p.lang,
            store,
            &p.tree,
            &p.raw,
            &p.ids,
            &env,
        )?;
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
    let ids = assign_ids(&raw, &path, &mut prev_ids(prev), prev);
    let mut env = extra.clone();
    for (i, ent) in raw.iter().enumerate() {
        if is_inherent_raw(&raw, i) {
            continue;
        }
        env.insert_def_in(&ent.name, ent.kind, ids[i], Some(&path));
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
    // Clone once per file: `Env.names` is Arc, so this is not O(n) in the snapshot.
    // Filling `self_methods` only when the caller left it empty — `edit_def` fills it
    // from the snapshot, and the fragment being parsed has no siblings (opus 03:05).
    let mut local_env = env.clone();
    local_env.current_file = Some(path.clone());
    let caller_supplied = !env.self_methods.is_empty();
    let mut sibs: std::collections::HashMap<usize, std::collections::HashMap<String, EntityId>> =
        std::collections::HashMap::new();
    if !caller_supplied {
        for (j, sib) in raw.iter().enumerate() {
            if let Some(p) = sib.parent_idx {
                if is_callable_member(sib.kind) {
                    sibs.entry(p).or_default().insert(sib.name.clone(), ids[j]);
                }
            }
        }
    }
    for (i, ent) in raw.iter().enumerate() {
        let node = extract::find_node(tree.root_node(), ent.item_range)
            .ok_or_else(|| Error::Parse(format!("no node for {}", ent.name)))?;
        if !caller_supplied {
            local_env.self_methods = ent
                .parent_idx
                .and_then(|p| sibs.get(&p))
                .cloned()
                .unwrap_or_default();
        }
        let res = resolve(node, src, lang, &local_env)?;
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
        let content = canonicalize(node, src, &res, &child_spans, &local_env, lang)?;
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

/// Reuse ids from `prev` by SigKey: nested items match under their parent, file-level
/// items only within `file` (two files may each define `fn hex32`).
/// The previous snapshot's ids by signature, each list in id order: a re-ingested entity
/// keeps its id, and two same-signature entities take theirs in the order they had.
/// Built once per ingest; a lookup per raw entity instead of a scan of every record.
type PrevIds = BTreeMap<SigKey, std::collections::VecDeque<EntityId>>;

fn prev_ids(prev: Option<&Snapshot>) -> PrevIds {
    let mut by_sig = PrevIds::new();
    if let Some(prev) = prev {
        for (id, rec) in &prev.entities {
            by_sig.entry(rec.sig_key()).or_default().push_back(*id);
        }
    }
    by_sig
}

fn assign_ids(
    raw: &[RawEntity],
    file: &RelPath,
    prev: &mut PrevIds,
    snap: Option<&Snapshot>,
) -> Vec<EntityId> {
    let mut assigned: Vec<Option<EntityId>> = vec![None; raw.len()];
    let mut used = HashSet::new();
    for (i, ent) in raw.iter().enumerate() {
        let parent = match ent.parent_idx {
            Some(p) => match assigned[p] {
                Some(id) => Some(id),
                None => continue,
            },
            None => None,
        };
        let key = SigKey::new(parent, file, ent.kind, ent.name.clone());
        if let Some(id) = prev.get_mut(&key).and_then(|same| same.pop_front()) {
            assigned[i] = Some(id);
            used.insert(id);
        }
    }
    // A `use` line's name is its text. Rename of an imported fn (or a hand
    // edit of the path) changes that spelling, so SigKey misses and the line
    // used to mint a new id. Reuse an unused Opaque in this file whose text
    // still shares most of its prefix (`use crate::a::f` → `use crate::a::f2`).
    if let Some(snap) = snap {
        let leftover: Vec<(u32, EntityId, String)> = snap
            .entities
            .iter()
            .filter(|(id, rec)| rec.file == *file && rec.kind == Kind::Opaque && !used.contains(id))
            .map(|(id, rec)| (rec.ordinal, *id, rec.name.clone()))
            .collect();
        for (i, ent) in raw.iter().enumerate() {
            if assigned[i].is_some() || ent.kind != Kind::Opaque {
                continue;
            }
            let mut best: Option<(usize, u32, EntityId)> = None;
            for (ord, id, name) in &leftover {
                if used.contains(id) {
                    continue;
                }
                let n = lcp(&ent.name, name);
                if n * 2 < ent.name.len().min(name.len()) {
                    continue;
                }
                match best {
                    Some((bn, bord, _)) if (n, std::cmp::Reverse(*ord)) < (bn, std::cmp::Reverse(bord)) => {}
                    _ => best = Some((n, *ord, *id)),
                }
            }
            if let Some((_, _, id)) = best {
                assigned[i] = Some(id);
                used.insert(id);
            }
        }
    }
    assigned
        .into_iter()
        .map(|id| id.unwrap_or_else(EntityId::new))
        .collect()
}

fn lcp(a: &str, b: &str) -> usize {
    a.bytes()
        .zip(b.bytes())
        .take_while(|(x, y)| x == y)
        .count()
}
