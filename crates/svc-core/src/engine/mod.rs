use std::collections::{BTreeMap, HashMap, HashSet};

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
        insert_mod_child_rec(&mut env, snapshot, *id, rec);
        if is_inherent_rec(snapshot, rec) || is_block_local_rec(snapshot, rec) {
            continue;
        }
        env.insert_def_in(&rec.name, rec.kind, *id, Some(&rec.file));
    }
    link_file_modules_from_snapshot(&mut env, snapshot);
    fill_reexports_from_snapshot(&mut env, snapshot);
    fill_file_imports_from_snapshot(&mut env, snapshot);
    fill_mod_imports_from_snapshot(&mut env, snapshot);
    env
}

pub(crate) fn env_from_snapshot_store(snapshot: &Snapshot, store: &dyn Store) -> Env {
    let mut env = env_from_snapshot(snapshot);
    fill_path_file_modules_from_snapshot(&mut env, snapshot, store);
    fill_include_file_modules_from_snapshot(&mut env, snapshot, store);
    fill_file_module_includes_from_snapshot(&mut env, snapshot, store);
    fill_macro_exports_from_snapshot(&mut env, snapshot, store);
    env
}

fn rec_src(store: &dyn Store, rec: &EntityRecord) -> Option<String> {
    store
        .get_bytes_blob(rec.bytes)
        .ok()
        .and_then(|b| String::from_utf8(b.src().to_vec()).ok())
}

/// Postcard cannot persist `#[path]`. Recover the file from stored bytes so two
/// leftover `#[path]` modules in one directory are not left unattached.
fn fill_path_file_modules_from_snapshot(env: &mut Env, snapshot: &Snapshot, store: &dyn Store) {
    for (id, rec) in &snapshot.entities {
        if rec.kind != Kind::Mod {
            continue;
        }
        if snapshot.entities.values().any(|c| c.parent == Some(*id)) {
            continue;
        }
        let Some(src) = rec_src(store, rec) else {
            continue;
        };
        let Some(attr) = extract::bytes_path_attr(&src) else {
            continue;
        };
        let Some(cand) = resolve_path_attr(&rec.file, &attr) else {
            continue;
        };
        if !snapshot.files.contains_key(&cand) {
            continue;
        }
        env.file_of_mod.insert(cand.clone(), *id);
        env.mod_decl_file.insert(*id, rec.file.clone());
        for (cid, crec) in &snapshot.entities {
            if crec.file != cand {
                continue;
            }
            if crec.parent.is_some() || is_inherent_rec(snapshot, crec) {
                continue;
            }
            env.insert_mod_child(*id, &crec.name, crec.kind, *cid);
        }
    }
}

fn attach_included_file(
    env: &mut Env,
    snapshot: &Snapshot,
    mod_id: EntityId,
    decl: &RelPath,
    cand: RelPath,
) {
    if env
        .file_of_mod
        .get(&cand)
        .is_some_and(|existing| *existing != mod_id)
    {
        return;
    }
    env.file_of_mod.insert(cand.clone(), mod_id);
    env.mod_decl_file.insert(mod_id, decl.clone());
    for (cid, crec) in &snapshot.entities {
        if crec.file != cand {
            continue;
        }
        if crec.parent.is_some() || is_inherent_rec(snapshot, crec) {
            continue;
        }
        env.insert_mod_child(mod_id, &crec.name, crec.kind, *cid);
    }
}

/// `mod foo { include!("x.rs"); }` — postcard cannot persist the include path.
fn fill_include_file_modules_from_snapshot(
    env: &mut Env,
    snapshot: &Snapshot,
    store: &dyn Store,
) {
    for (id, rec) in &snapshot.entities {
        if rec.kind != Kind::Mod {
            continue;
        }
        let Some(src) = rec_src(store, rec) else {
            continue;
        };
        for inc in extract::bytes_include_paths(&src) {
            let Some(cand) = resolve_path_attr(&rec.file, &inc) else {
                continue;
            };
            if !snapshot.files.contains_key(&cand) {
                continue;
            }
            attach_included_file(env, snapshot, *id, &rec.file, cand);
        }
    }
}

/// `mod foo;` file with a file-root `include!("body.rs")` — postcard has no
/// include list on the file, so recover from stored bytes.
fn fill_file_module_includes_from_snapshot(
    env: &mut Env,
    snapshot: &Snapshot,
    store: &dyn Store,
) {
    let fom = env.file_of_mod.clone();
    for (path, mod_id) in fom {
        if path.extension() != Some("rs") {
            continue;
        }
        let src = approx_file_src(snapshot, store, &path);
        if src.is_empty() {
            continue;
        }
        let Ok(tree) = parse(&src, &crate::RustLang) else {
            continue;
        };
        for inc in extract::file_include_paths(tree.root_node(), &src) {
            let Some(cand) = resolve_path_attr(&path, &inc) else {
                continue;
            };
            if !snapshot.files.contains_key(&cand) {
                continue;
            }
            attach_included_file(env, snapshot, mod_id, &path, cand);
        }
    }
}

fn approx_file_src(snapshot: &Snapshot, store: &dyn Store, path: &RelPath) -> Vec<u8> {
    let mut items: Vec<_> = snapshot
        .entities
        .values()
        .filter(|r| r.file == *path && r.parent.is_none())
        .collect();
    items.sort_by_key(|r| r.ordinal);
    let mut out = Vec::new();
    for rec in items {
        if let Some(s) = rec_src(store, rec) {
            out.extend_from_slice(s.as_bytes());
        }
    }
    if let Some(fr) = snapshot.files.get(path) {
        if let Ok(t) = fr.tail(store) {
            out.extend(t);
        }
    }
    out
}

/// Postcard cannot persist `#[macro_export]` / `#[macro_use]`. Recover them from
/// the stored bytes (leading attrs sit in the item's extent).
fn fill_macro_exports_from_snapshot(env: &mut Env, snapshot: &Snapshot, store: &dyn Store) {
    let mut use_mods = HashMap::new();
    for (id, rec) in &snapshot.entities {
        if rec.kind != Kind::Mod {
            continue;
        }
        let Some(src) = rec_src(store, rec) else {
            continue;
        };
        if let Some(only) = extract::bytes_macro_use_spec(&src) {
            use_mods.insert(*id, only);
        }
    }
    for (id, rec) in &snapshot.entities {
        if rec.kind != Kind::Macro {
            continue;
        }
        let src = rec_src(store, rec).unwrap_or_default();
        let from_export = extract::bytes_has_macro_export(&src);
        let from_parent = rec.parent.is_some_and(|p| {
            use_mods
                .get(&p)
                .is_some_and(|only| macro_use_allows(only, &rec.name))
        });
        if from_export || from_parent {
            env.export_macro(&rec.name, *id);
        }
    }
    let file_of_mod = env.file_of_mod.clone();
    for (path, mid) in file_of_mod {
        let Some(only) = use_mods.get(&mid) else {
            continue;
        };
        for (id, rec) in &snapshot.entities {
            if rec.kind == Kind::Macro
                && rec.parent.is_none()
                && rec.file == path
                && macro_use_allows(only, &rec.name)
            {
                env.export_macro(&rec.name, *id);
            }
        }
    }
}

fn fill_reexports_from_snapshot(env: &mut Env, snapshot: &Snapshot) {
    env.bind_reexports = true;
    let rust = crate::RustLang;
    for rec in snapshot.entities.values() {
        if rec.kind != Kind::Opaque || !rec.name.contains("pub use") {
            continue;
        }
        env.current_file = Some(rec.file.clone());
        env.in_nested_mod = false;
        env.self_mod = None;
        env.inline_mod = false;
        apply_file_module_env(env);
        if let Some(p) = rec.parent {
            if snapshot.entities.get(&p).is_some_and(|r| r.kind == Kind::Mod) {
                env.self_mod = Some(p);
                env.inline_mod = true;
                env.in_nested_mod = true;
            }
        }
        let src = rec.name.as_bytes();
        if let Ok(tree) = parse(src, &rust) {
            canon::collect_use_imports(env, tree.root_node(), src);
        }
    }
    env.bind_reexports = false;
    env.current_file = None;
    env.in_nested_mod = false;
    env.self_mod = None;
    env.inline_mod = false;
    env.super_files.clear();
    env.super_stack.clear();
}

fn fill_file_imports_from_snapshot(env: &mut Env, snapshot: &Snapshot) {
    let rust = crate::RustLang;
    for rec in snapshot.entities.values() {
        if rec.kind != Kind::Opaque || !rec.name.contains("use ") {
            continue;
        }
        if rec.parent.is_some() {
            continue;
        }
        env.current_file = Some(rec.file.clone());
        env.in_nested_mod = false;
        env.self_mod = None;
        env.inline_mod = false;
        apply_file_module_env(env);
        let src = rec.name.as_bytes();
        if let Ok(tree) = parse(src, &rust) {
            canon::collect_use_imports(env, tree.root_node(), src);
        }
    }
    env.use_imports.clear();
    env.use_aliases.clear();
    env.current_file = None;
    env.in_nested_mod = false;
    env.self_mod = None;
    env.inline_mod = false;
    env.super_files.clear();
    env.super_stack.clear();
}

fn fill_mod_imports_from_snapshot(env: &mut Env, snapshot: &Snapshot) {
    let rust = crate::RustLang;
    for rec in snapshot.entities.values() {
        if rec.kind != Kind::Opaque || !rec.name.contains("use ") {
            continue;
        }
        let Some(p) = rec.parent else {
            continue;
        };
        if !snapshot.entities.get(&p).is_some_and(|r| r.kind == Kind::Mod) {
            continue;
        }
        env.current_file = Some(rec.file.clone());
        env.in_nested_mod = false;
        env.self_mod = None;
        env.inline_mod = false;
        apply_file_module_env(env);
        env.self_mod = Some(p);
        env.in_nested_mod = true;
        env.inline_mod = true;
        let src = rec.name.as_bytes();
        if let Ok(tree) = parse(src, &rust) {
            canon::collect_use_imports(env, tree.root_node(), src);
        }
    }
    env.use_imports.clear();
    env.use_aliases.clear();
    env.current_file = None;
    env.in_nested_mod = false;
    env.self_mod = None;
    env.inline_mod = false;
    env.super_files.clear();
    env.super_stack.clear();
}

/// Inherent methods nested under `parent` (an `impl` or class), and associated
/// consts/statics so `Self::N` binds like `Self::foo()`. Empty when the item
/// is file-root: there is no receiver to bind against.
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
    matches!(
        kind,
        Kind::Fn | Kind::Const | Kind::Static | Kind::JsMethod | Kind::JsStaticMethod
    )
}

/// Items nested under `impl`/`trait`/`class` stay out of the file/crate maps.
/// Methods live in `self_methods` (`read()` is a free fn, `self.read()` is the
/// method). Associated consts/types would otherwise collide with a unique
/// crate-level `parse` the way nested fns and `#[cfg(test)] mod tests` did.
fn is_inherent_member(kind: Kind, parent_kind: Kind) -> bool {
    match parent_kind {
        Kind::Impl | Kind::Trait => matches!(
            kind,
            Kind::Fn | Kind::Const | Kind::TypeAlias | Kind::Static | Kind::Macro
        ),
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

/// A function body (and JS function/method body) can host nested items.
/// So can a `mod`: `#[cfg(test)] mod tests { fn parse() {} }` must not occupy
/// the file/crate maps (M3). Those names are in [`Env::nested_items`] while
/// that body — or a sibling in the same mod — is resolved.
fn hosts_block_items(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Fn
            | Kind::Mod
            | Kind::JsFunction
            | Kind::JsMethod
            | Kind::JsGetter
            | Kind::JsSetter
    )
}

fn is_block_local_rec(snapshot: &Snapshot, rec: &EntityRecord) -> bool {
    rec.parent.is_some_and(|p| {
        snapshot
            .entities
            .get(&p)
            .is_some_and(|par| hosts_block_items(par.kind))
    })
}

fn is_block_local_raw(raw: &[RawEntity], i: usize) -> bool {
    raw[i]
        .parent_idx
        .is_some_and(|p| hosts_block_items(raw[p].kind))
}

fn macro_use_allows(only: &Option<Vec<String>>, name: &str) -> bool {
    match only {
        None => true,
        Some(names) => names.iter().any(|n| n == name),
    }
}

fn export_macro_raw(env: &mut Env, raw: &[RawEntity], i: usize, id: EntityId) {
    if raw[i].kind != Kind::Macro {
        return;
    }
    let from_parent = raw[i].parent_idx.is_some_and(|p| {
        raw[p].kind == Kind::Mod
            && raw[p].macro_use
            && macro_use_allows(&raw[p].macro_use_only, &raw[i].name)
    });
    if raw[i].macro_export || from_parent {
        env.export_macro(&raw[i].name, id);
    }
}

fn export_file_module_macro_use(
    env: &mut Env,
    files: &[(&RelPath, &[RawEntity], &[EntityId])],
) {
    let mut use_mods = HashMap::new();
    for (_, raw, ids) in files {
        for (i, ent) in raw.iter().enumerate() {
            if ent.kind == Kind::Mod && ent.macro_use {
                use_mods.insert(ids[i], ent.macro_use_only.clone());
            }
        }
    }
    for (path, raw, ids) in files {
        let Some(&mid) = env.file_of_mod.get(*path) else {
            continue;
        };
        let Some(only) = use_mods.get(&mid) else {
            continue;
        };
        for (i, ent) in raw.iter().enumerate() {
            if ent.kind == Kind::Macro
                && ent.parent_idx.is_none()
                && macro_use_allows(only, &ent.name)
            {
                env.export_macro(&ent.name, ids[i]);
            }
        }
    }
}

fn skip_crate_occupy_raw(raw: &[RawEntity], i: usize) -> bool {
    if is_inherent_raw(raw, i) {
        return true;
    }
    if !is_block_local_raw(raw, i) {
        return false;
    }
    !(raw[i].kind == Kind::Macro && raw[i].macro_export)
}

fn insert_mod_child_raw(env: &mut Env, raw: &[RawEntity], ids: &[EntityId], i: usize) {
    let Some(pi) = raw[i].parent_idx else {
        return;
    };
    if raw[pi].kind != Kind::Mod || is_inherent_raw(raw, i) {
        return;
    }
    env.insert_mod_child(ids[pi], &raw[i].name, raw[i].kind, ids[i]);
}

fn insert_mod_child_rec(env: &mut Env, snapshot: &Snapshot, id: EntityId, rec: &EntityRecord) {
    let Some(p) = rec.parent else {
        return;
    };
    let Some(prec) = snapshot.entities.get(&p) else {
        return;
    };
    if prec.kind != Kind::Mod || is_inherent_rec(snapshot, rec) {
        return;
    }
    env.insert_mod_child(p, &rec.name, rec.kind, id);
}

/// Ancestors that are `mod outer { … }` in the same file — rustc loads
/// `mod inner;` there from `outer/inner.rs`, not `inner.rs` next to the file.
fn enclosing_inline_mods_raw(raw: &[RawEntity], i: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut walk = raw[i].parent_idx;
    while let Some(p) = walk {
        if raw[p].kind == Kind::Mod {
            names.push(raw[p].name.clone());
        }
        walk = raw[p].parent_idx;
    }
    names.reverse();
    names
}

fn enclosing_inline_mods_rec(snapshot: &Snapshot, id: EntityId) -> Vec<String> {
    let mut names = Vec::new();
    let mut walk = snapshot.entities.get(&id).and_then(|r| r.parent);
    while let Some(pid) = walk {
        let Some(prec) = snapshot.entities.get(&pid) else {
            break;
        };
        if prec.kind == Kind::Mod {
            names.push(prec.name.clone());
        }
        walk = prec.parent;
    }
    names.reverse();
    names
}

/// `mod foo;` in `src/lib.rs` loads `src/foo.rs` or `src/foo/mod.rs`.
/// `mod outer { mod foo; }` in that file loads `src/outer/foo.rs`.
fn file_module_paths(parent_file: &RelPath, inline: &[String], name: &str) -> Vec<RelPath> {
    let path = parent_file.as_str();
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    let mut base = if matches!(stem, "mod" | "lib" | "main") {
        dir.to_string()
    } else if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{dir}/{stem}")
    };
    for seg in inline {
        base = if base.is_empty() {
            seg.clone()
        } else {
            format!("{base}/{seg}")
        };
    }
    let rs = if base.is_empty() {
        format!("{name}.rs")
    } else {
        format!("{base}/{name}.rs")
    };
    let modrs = if base.is_empty() {
        format!("{name}/mod.rs")
    } else {
        format!("{base}/{name}/mod.rs")
    };
    [&rs, &modrs]
        .into_iter()
        .filter_map(|p| RelPath::new(p.to_string()).ok())
        .collect()
}

fn resolve_path_attr(parent_file: &RelPath, attr: &str) -> Option<RelPath> {
    let attr = attr.trim().trim_start_matches("./");
    if attr.is_empty() {
        return None;
    }
    let path = parent_file.as_str();
    let dir = match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    };
    let joined = if dir.is_empty() {
        attr.to_string()
    } else {
        format!("{dir}/{attr}")
    };
    RelPath::new(joined).ok()
}

fn same_dir(a: &RelPath, b: &RelPath) -> bool {
    let da = a.as_str().rfind('/').map(|i| &a.as_str()[..i]).unwrap_or("");
    let db = b.as_str().rfind('/').map(|i| &b.as_str()[..i]).unwrap_or("");
    da == db
}

fn link_file_modules(
    env: &mut Env,
    files: &[(&RelPath, &[RawEntity], &[EntityId])],
) {
    let mut file_of_mod: HashMap<RelPath, EntityId> = HashMap::new();
    for (path, raw, ids) in files {
        for (i, ent) in raw.iter().enumerate() {
            if ent.kind != Kind::Mod {
                continue;
            }
            let mut cands = Vec::new();
            if let Some(p) = ent
                .path_attr
                .as_deref()
                .and_then(|a| resolve_path_attr(path, a))
            {
                cands.push(p);
            } else if ent.children.is_empty() && ent.include_paths.is_empty() {
                cands.extend(file_module_paths(
                    path,
                    &enclosing_inline_mods_raw(raw, i),
                    &ent.name,
                ));
            }
            for inc in &ent.include_paths {
                if let Some(p) = resolve_path_attr(path, inc) {
                    cands.push(p);
                }
            }
            for cand in cands {
                file_of_mod.insert(cand, ids[i]);
                env.mod_decl_file.insert(ids[i], (*path).clone());
            }
        }
    }
    for (path, raw, ids) in files {
        let Some(&mod_id) = file_of_mod.get(*path) else {
            continue;
        };
        for (i, ent) in raw.iter().enumerate() {
            if is_inherent_raw(raw, i) || is_block_local_raw(raw, i) {
                continue;
            }
            env.insert_mod_child(mod_id, &ent.name, ent.kind, ids[i]);
        }
    }
    env.file_of_mod.extend(file_of_mod);
}

/// `mod foo;` + `src/foo.rs` containing `include!("body.rs")` splices body
/// items into `foo`, same as `mod foo { include!("body.rs"); }`.
fn link_file_root_includes(
    env: &mut Env,
    files: &[(&RelPath, &[u8], &tree_sitter::Tree, &[RawEntity], &[EntityId])],
) {
    let by_path: HashMap<&RelPath, (&[RawEntity], &[EntityId])> = files
        .iter()
        .map(|(p, _, _, raw, ids)| (*p, (*raw, *ids)))
        .collect();
    let fom = env.file_of_mod.clone();
    for (path, src, tree, _, _) in files {
        let Some(&mod_id) = fom.get(*path) else {
            continue;
        };
        for inc in extract::file_include_paths(tree.root_node(), src) {
            let Some(cand) = resolve_path_attr(path, &inc) else {
                continue;
            };
            let Some((raw, ids)) = by_path.get(&cand) else {
                continue;
            };
            if env
                .file_of_mod
                .get(&cand)
                .is_some_and(|existing| *existing != mod_id)
            {
                continue;
            }
            env.file_of_mod.insert(cand.clone(), mod_id);
            env.mod_decl_file.insert(mod_id, (*path).clone());
            for (i, ent) in raw.iter().enumerate() {
                if ent.parent_idx.is_some()
                    || is_inherent_raw(raw, i)
                    || is_block_local_raw(raw, i)
                {
                    continue;
                }
                env.insert_mod_child(mod_id, &ent.name, ent.kind, ids[i]);
            }
        }
    }
}

fn link_file_modules_from_raw(env: &mut Env, path: &RelPath, raw: &[RawEntity], ids: &[EntityId]) {
    link_file_modules(env, &[(path, raw, ids)]);
}

fn link_file_modules_from_snapshot(env: &mut Env, snapshot: &Snapshot) {
    let mut file_of_mod: HashMap<RelPath, EntityId> = HashMap::new();
    for (id, rec) in &snapshot.entities {
        if rec.kind != Kind::Mod {
            continue;
        }
        if snapshot.entities.values().any(|c| c.parent == Some(*id)) {
            continue;
        }
        for cand in file_module_paths(
            &rec.file,
            &enclosing_inline_mods_rec(snapshot, *id),
            &rec.name,
        ) {
            file_of_mod.insert(cand, *id);
            env.mod_decl_file.insert(*id, rec.file.clone());
        }
    }
    let present: HashSet<_> = snapshot.files.keys().cloned().collect();
    let claimed: HashSet<_> = file_of_mod
        .keys()
        .filter(|p| present.contains(*p))
        .cloned()
        .collect();
    for (id, rec) in &snapshot.entities {
        if rec.kind != Kind::Mod {
            continue;
        }
        if snapshot.entities.values().any(|c| c.parent == Some(*id)) {
            continue;
        }
        let std = file_module_paths(
            &rec.file,
            &enclosing_inline_mods_rec(snapshot, *id),
            &rec.name,
        );
        if std.iter().any(|p| present.contains(p)) {
            continue;
        }
        let leftovers: Vec<_> = present
            .iter()
            .filter(|f| {
                *f != &rec.file
                    && !claimed.contains(*f)
                    && same_dir(f, &rec.file)
            })
            .cloned()
            .collect();
        if leftovers.len() == 1 {
            file_of_mod.insert(leftovers[0].clone(), *id);
            env.mod_decl_file.insert(*id, rec.file.clone());
        }
    }
    for (id, rec) in &snapshot.entities {
        let Some(&mod_id) = file_of_mod.get(&rec.file) else {
            continue;
        };
        if rec.parent.is_some() || is_inherent_rec(snapshot, rec) {
            continue;
        }
        env.insert_mod_child(mod_id, &rec.name, rec.kind, *id);
    }
    env.file_of_mod.extend(file_of_mod);
}

fn nearest_mod_raw(raw: &[RawEntity], i: usize) -> Option<usize> {
    let mut walk = raw[i].parent_idx;
    while let Some(p) = walk {
        if raw[p].kind == Kind::Mod {
            return Some(p);
        }
        walk = raw[p].parent_idx;
    }
    None
}

fn nearest_mod_rec(snapshot: &Snapshot, id: EntityId) -> Option<EntityId> {
    let mut walk = snapshot.entities.get(&id).and_then(|r| r.parent);
    while let Some(pid) = walk {
        let Some(prec) = snapshot.entities.get(&pid) else {
            break;
        };
        if prec.kind == Kind::Mod {
            return Some(pid);
        }
        walk = prec.parent;
    }
    None
}

fn apply_file_module_env(env: &mut Env) {
    let Some(file) = env.current_file.clone() else {
        return;
    };
    fill_super_files(env, &file);
    let Some(&m) = env.file_of_mod.get(&file) else {
        return;
    };
    env.in_nested_mod = true;
    env.self_mod = Some(m);
}

/// `mod outer { mod inner; }` — `super::` in `inner.rs` is `outer`'s items, not
/// the declaring file's crate-root names.
fn fill_file_module_inline_supers(env: &mut Env) {
    let Some(mut walk) = env.self_mod else {
        return;
    };
    let file_mods: HashSet<_> = env.file_of_mod.values().copied().collect();
    while let Some(&p) = env.mod_parent.get(&walk) {
        if file_mods.contains(&p) {
            break;
        }
        let mut map = HashMap::new();
        if let Some(items) = env.mod_items.get(&p) {
            map.extend(items.clone());
        }
        if let Some(im) = env.mod_imports.get(&p) {
            map.extend(im.clone());
        }
        if let Some(re) = env.mod_reexports.get(&p) {
            map.extend(re.clone());
        }
        env.super_stack.push(map);
        walk = p;
    }
}

fn fill_super_files(env: &mut Env, start: &RelPath) {
    env.super_files.clear();
    let mut walk = start.clone();
    let mut seen = HashSet::new();
    while let Some(&mod_id) = env.file_of_mod.get(&walk) {
        if !seen.insert(mod_id) {
            break;
        }
        let Some(decl) = env.mod_decl_file.get(&mod_id).cloned() else {
            break;
        };
        env.super_files.push(decl.clone());
        walk = decl;
    }
}

fn fill_mod_env_from_raw(env: &mut Env, raw: &[RawEntity], ids: &[EntityId], i: usize) {
    env.in_nested_mod = false;
    env.super_stack.clear();
    env.self_mod = None;
    env.inline_mod = false;
    apply_file_module_env(env);
    let Some(mut m) = nearest_mod_raw(raw, i) else {
        fill_file_module_inline_supers(env);
        return;
    };
    env.in_nested_mod = true;
    env.inline_mod = true;
    env.self_mod = Some(ids[m]);
    loop {
        let Some(pp) = raw[m].parent_idx else {
            break;
        };
        if raw[pp].kind != Kind::Mod {
            break;
        }
        let mut map = HashMap::new();
        for (j, ch) in raw.iter().enumerate() {
            if ch.parent_idx == Some(pp) && is_block_local_raw(raw, j) {
                Env::insert_super_level(&mut map, &ch.name, ch.kind, ids[j]);
            }
        }
        if let Some(im) = env.mod_imports.get(&ids[pp]) {
            map.extend(im.clone());
        }
        if let Some(re) = env.mod_reexports.get(&ids[pp]) {
            map.extend(re.clone());
        }
        env.super_stack.push(map);
        m = pp;
    }
}

pub(crate) fn fill_mod_env_from_snapshot(env: &mut Env, snapshot: &Snapshot, id: EntityId) {
    env.in_nested_mod = false;
    env.super_stack.clear();
    env.self_mod = None;
    env.inline_mod = false;
    apply_file_module_env(env);
    let Some(mut m) = nearest_mod_rec(snapshot, id) else {
        fill_file_module_inline_supers(env);
        return;
    };
    env.in_nested_mod = true;
    env.inline_mod = true;
    env.self_mod = Some(m);
    loop {
        let Some(pp) = snapshot.entities.get(&m).and_then(|r| r.parent) else {
            break;
        };
        let Some(prec) = snapshot.entities.get(&pp) else {
            break;
        };
        if prec.kind != Kind::Mod {
            break;
        }
        let mut map = HashMap::new();
        for (cid, crec) in &snapshot.entities {
            if crec.parent == Some(pp) && is_block_local_rec(snapshot, crec) {
                Env::insert_super_level(&mut map, &crec.name, crec.kind, *cid);
            }
        }
        if let Some(im) = env.mod_imports.get(&pp) {
            map.extend(im.clone());
        }
        if let Some(re) = env.mod_reexports.get(&pp) {
            map.extend(re.clone());
        }
        env.super_stack.push(map);
        m = pp;
    }
}

pub(crate) fn fill_use_imports_from_snapshot(
    env: &mut Env,
    snapshot: &Snapshot,
    file: &RelPath,
    langs: &Langs,
) {
    env.use_imports.clear();
    env.use_aliases.clear();
    let Some(lang) = langs.for_path(file) else {
        return;
    };
    for rec in snapshot.entities.values() {
        if rec.file != *file || rec.kind != Kind::Opaque {
            continue;
        }
        if !rec.name.contains("use ") {
            continue;
        }
        if env.inline_mod {
            if rec.parent != env.self_mod {
                continue;
            }
        } else if rec.parent.is_some() {
            continue;
        }
        let src = rec.name.as_bytes();
        let Ok(tree) = parse(src, lang) else {
            continue;
        };
        canon::collect_use_imports(env, tree.root_node(), src);
    }
}

pub(crate) fn fill_nested_use_imports(
    env: &mut Env,
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
) {
    if lang.name() != "rust" {
        return;
    }
    canon::collect_nested_use_imports(env, node, src);
}

/// Associated types of the enclosing impl/trait are in scope for signatures
/// (`fn f() -> Item`) without occupying the file map. Methods stay out: a
/// bare `f()` is not the sibling method.
fn fill_associated_types_from_snapshot(env: &mut Env, snapshot: &Snapshot, id: EntityId) {
    let Some(rec) = snapshot.entities.get(&id) else {
        return;
    };
    let host = if matches!(rec.kind, Kind::Impl | Kind::Trait) {
        id
    } else {
        let Some(parent) = rec.parent else {
            return;
        };
        parent
    };
    let Some(hrec) = snapshot.entities.get(&host) else {
        return;
    };
    if !matches!(hrec.kind, Kind::Impl | Kind::Trait) {
        return;
    }
    for (cid, crec) in &snapshot.entities {
        if crec.parent == Some(host) && crec.kind == Kind::TypeAlias {
            env.insert_nested(&crec.name, crec.kind, *cid);
        }
    }
}

fn fill_associated_types_from_raw(env: &mut Env, raw: &[RawEntity], ids: &[EntityId], i: usize) {
    let host = if matches!(raw[i].kind, Kind::Impl | Kind::Trait) {
        i
    } else {
        let Some(p) = raw[i].parent_idx else {
            return;
        };
        p
    };
    if !matches!(raw[host].kind, Kind::Impl | Kind::Trait) {
        return;
    }
    for (j, ch) in raw.iter().enumerate() {
        if ch.parent_idx == Some(host) && ch.kind == Kind::TypeAlias {
            env.insert_nested(&ch.name, ch.kind, ids[j]);
        }
    }
}

/// Nested `fn`/`struct`/… under `id` and under enclosing functions, inner last.
pub(crate) fn fill_nested_items_from_snapshot(env: &mut Env, snapshot: &Snapshot, id: EntityId) {
    env.nested_items.clear();
    fill_associated_types_from_snapshot(env, snapshot, id);
    let mut chain = vec![id];
    let mut walk = snapshot.entities.get(&id).and_then(|r| r.parent);
    while let Some(pid) = walk {
        let Some(prec) = snapshot.entities.get(&pid) else {
            break;
        };
        if prec.kind == Kind::Mod {
            chain.push(pid);
            break;
        }
        if hosts_block_items(prec.kind) {
            chain.push(pid);
        }
        walk = prec.parent;
    }
    chain.reverse();
    for pid in chain {
        for (cid, crec) in &snapshot.entities {
            if crec.parent == Some(pid) && is_block_local_rec(snapshot, crec) {
                env.insert_nested(&crec.name, crec.kind, *cid);
            }
        }
    }
}

fn fill_nested_items_from_raw(env: &mut Env, raw: &[RawEntity], ids: &[EntityId], i: usize) {
    env.nested_items.clear();
    fill_associated_types_from_raw(env, raw, ids, i);
    let mut chain = vec![i];
    let mut walk = raw[i].parent_idx;
    while let Some(pi) = walk {
        if raw[pi].kind == Kind::Mod {
            chain.push(pi);
            break;
        }
        if hosts_block_items(raw[pi].kind) {
            chain.push(pi);
        }
        walk = raw[pi].parent_idx;
    }
    chain.reverse();
    for pi in chain {
        for (j, ch) in raw.iter().enumerate() {
            if ch.parent_idx == Some(pi) && is_block_local_raw(raw, j) {
                env.insert_nested(&ch.name, ch.kind, ids[j]);
            }
        }
    }
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
    bytes::bytes_from_span(src, extent, children, name, resolution, item)
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
    snapshot_files_reusing(store, langs, files, prev, change, &std::collections::BTreeSet::new())
}

/// [`snapshot_files`] where `unchanged` names files whose bytes are exactly what `prev`
/// rendered for them. Parsing is cheap; `materialize` (the canonical stream, resolved
/// against the whole tree's names) is what an absorb of one file pays for every file.
/// When the tree's definitions — name, kind, id, parent, file — are the same set as
/// `prev`'s, the names resolve exactly as they did, so an unchanged file's records are
/// `prev`'s records: same content, same bytes, same ids. Any new, gone, renamed or moved
/// definition anywhere re-materializes everything, as before.
pub fn snapshot_files_reusing(
    store: &dyn Store,
    langs: &Langs,
    files: &BTreeMap<RelPath, Vec<u8>>,
    prev: Option<&Snapshot>,
    change: ChangeId,
    unchanged: &std::collections::BTreeSet<RelPath>,
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
            insert_mod_child_raw(&mut env, &p.raw, &p.ids, i);
            export_macro_raw(&mut env, &p.raw, i, p.ids[i]);
            if skip_crate_occupy_raw(&p.raw, i) {
                continue;
            }
            env.insert_def_in(&ent.name, ent.kind, p.ids[i], Some(&p.path));
        }
    }
    {
        let views: Vec<(&RelPath, &[RawEntity], &[EntityId])> = parsed
            .iter()
            .map(|p| (&p.path, p.raw.as_slice(), p.ids.as_slice()))
            .collect();
        link_file_modules(&mut env, &views);
        let include_views: Vec<(&RelPath, &[u8], &tree_sitter::Tree, &[RawEntity], &[EntityId])> =
            parsed
                .iter()
                .map(|p| {
                    (
                        &p.path,
                        p.src,
                        &p.tree,
                        p.raw.as_slice(),
                        p.ids.as_slice(),
                    )
                })
                .collect();
        link_file_root_includes(&mut env, &include_views);
        export_file_module_macro_use(&mut env, &views);
    }
    {
        env.bind_reexports = true;
        for p in &parsed {
            env.current_file = Some(p.path.clone());
            env.in_nested_mod = false;
            env.self_mod = None;
            env.inline_mod = false;
            apply_file_module_env(&mut env);
            canon::collect_use_imports(&mut env, p.tree.root_node(), p.src);
        }
        env.bind_reexports = false;
        for p in &parsed {
            env.current_file = Some(p.path.clone());
            env.in_nested_mod = false;
            env.self_mod = None;
            env.inline_mod = false;
            apply_file_module_env(&mut env);
            env.use_imports.clear();
            env.use_aliases.clear();
            canon::collect_use_imports(&mut env, p.tree.root_node(), p.src);
        }
        for p in &parsed {
            env.current_file = Some(p.path.clone());
            apply_file_module_env(&mut env);
            for ent in p.raw.iter() {
                if ent.kind != Kind::Opaque || !ent.name.contains("use ") {
                    continue;
                }
                let Some(pi) = ent.parent_idx else {
                    continue;
                };
                if p.raw[pi].kind != Kind::Mod {
                    continue;
                }
                env.self_mod = Some(p.ids[pi]);
                env.in_nested_mod = true;
                env.inline_mod = true;
                env.use_imports.clear();
                env.use_aliases.clear();
                let src = ent.name.as_bytes();
                if let Ok(tree) = parse(src, p.lang) {
                    canon::collect_use_imports(&mut env, tree.root_node(), src);
                }
            }
        }
        env.use_imports.clear();
        env.use_aliases.clear();
        env.current_file = None;
        env.in_nested_mod = false;
        env.self_mod = None;
        env.inline_mod = false;
        env.super_files.clear();
        env.super_stack.clear();
    }
    // The definitions this tree declares, as `prev` would list them; equal sets mean an
    // identical name environment.
    let reuse = match prev {
        Some(prev) if !unchanged.is_empty() => {
            let mut now: Vec<(&RelPath, &str, Kind, EntityId, Option<EntityId>)> = parsed
                .iter()
                .flat_map(|p| {
                    p.raw.iter().enumerate().map(move |(i, ent)| {
                        (&p.path, ent.name.as_str(), ent.kind, p.ids[i], ent.parent_idx.map(|pi| p.ids[pi]))
                    })
                })
                .collect();
            let mut before: Vec<(&RelPath, &str, Kind, EntityId, Option<EntityId>)> = prev
                .entities
                .iter()
                .map(|(id, rec)| (&rec.file, rec.name.as_str(), rec.kind, *id, rec.parent))
                .collect();
            now.sort();
            before.sort();
            now == before
        }
        _ => false,
    };
    let mut entities = BTreeMap::new();
    let mut file_recs = BTreeMap::new();
    for p in &parsed {
        if reuse && unchanged.contains(&p.path) {
            if let Some(prev) = prev
                && let Some(file) = prev.files.get(&p.path)
            {
                entities.extend(prev.entities.iter().filter(|(_, r)| r.file == p.path).map(|(id, r)| (*id, r.clone())));
                file_recs.insert(p.path.clone(), file.clone());
                continue;
            }
        }
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
        file_recs.insert(path, FileRecord::from_tail(store, &src)?);
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
        insert_mod_child_raw(&mut env, &raw, &ids, i);
        export_macro_raw(&mut env, &raw, i, ids[i]);
        if skip_crate_occupy_raw(&raw, i) {
            continue;
        }
        env.insert_def_in(&ent.name, ent.kind, ids[i], Some(&path));
    }
    link_file_modules_from_raw(&mut env, &path, &raw, &ids);
    export_file_module_macro_use(&mut env, &[(&path, raw.as_slice(), ids.as_slice())]);
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
            // Methods resolve against sibling callables on the enclosing impl.
            // The impl body itself also needs that map: soup `impl S { … }` is
            // a token_tree, so `Self::parse()` lives on the impl entity, not
            // on a spanning `function_item`.
            local_env.self_methods = if matches!(ent.kind, Kind::Impl | Kind::Trait) {
                sibs.get(&i).cloned().unwrap_or_default()
            } else {
                ent.parent_idx
                    .and_then(|p| sibs.get(&p))
                    .cloned()
                    .unwrap_or_default()
            };
        }
        fill_nested_items_from_raw(&mut local_env, raw, ids, i);
        fill_mod_env_from_raw(&mut local_env, raw, ids, i);
        canon::fill_use_imports(&mut local_env, node, src, lang);
        let res = resolve(node, src, lang, &local_env)?;
        let children: Vec<(ByteRange, EntityId)> = ent
            .children
            .iter()
            .map(|&c| (raw[c].bytes_range, ids[c]))
            .collect();
        let own_name = ent.name_range.map(|r| (r, EntityId::SELF));
        let bytes = bytes::bytes_from_span(src, ent.bytes_range, &children, own_name, &res, node)?;
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
    Ok((entities, FileRecord::from_tail(store, &trailing)?))
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
