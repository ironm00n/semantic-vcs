use std::collections::{BTreeMap, BTreeSet};

use crate::content::{Bytes, Chunk, Content, IdentRef, Token};
use crate::delta::{Delta, ObservedClass};
use crate::entity::{EntityRecord, SigKey};
use crate::error::{Error, Result};
use crate::ids::{ByteRange, ChangeId, EntityId, RelPath, resolve_spec};
use crate::lang::{Lang, Langs};
use crate::op::Intent;
use crate::snapshot::Snapshot;
use crate::store::Store;

use super::{
    classify, env_from_snapshot, fill_self_methods_from_snapshot, ingest_file_prev,
    ingest_file_with_env, parse, render, render_entity, snapshot_files,
};

pub fn lookup_name(snap: &Snapshot, name: &str) -> Result<EntityId> {
    let hits: Vec<_> = snap
        .entities
        .iter()
        .filter(|(_, r)| r.name == name)
        .map(|(id, _)| *id)
        .collect();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(Error::NotFound(name.into())),
        _ => Err(Error::Other(format!("ambiguous name {name}"))),
    }
}

pub fn lookup(snap: &Snapshot, spec: &str) -> Result<EntityId> {
    if let Ok(id) = lookup_name(snap, spec) {
        return Ok(id);
    }
    match resolve_spec(snap.entities.keys().copied(), |id| id.matches_spec(spec)) {
        Ok(id) => Ok(id),
        Err(hits) if hits.is_empty() => Err(Error::NotFound(spec.into())),
        Err(_) => Err(Error::Other(format!("ambiguous entity {spec}"))),
    }
}

/// Attribute-only: referrers keep `Chunk::Name` holes. No rehash.
pub fn rename(snap: &Snapshot, id: EntityId, new: &str) -> Result<Snapshot> {
    let mut next = snap.clone();
    next.rename(id, new)?;
    Ok(next)
}

pub fn relocate(snap: &Snapshot, id: EntityId, file: RelPath, ordinal: u32) -> Result<Snapshot> {
    let mut next = snap.clone();
    // Render iterates `snapshot.files`, not entities. A path that never had a
    // FileRecord would swallow the item on disk even though the entity moved.
    next.ensure_file(file.clone());
    next.set_file(id, file, ordinal)?;
    Ok(next)
}

pub fn move_def(
    store: &dyn Store,
    langs: &Langs,
    snap: &Snapshot,
    id: EntityId,
    new_parent: Option<EntityId>,
    ordinal: Option<u32>,
) -> Result<Snapshot> {
    let rec = snap
        .entities
        .get(&id)
        .ok_or(Error::NoSuchEntity(id))?
        .clone();
    if let Some(p) = new_parent {
        if p == id || subtree(snap, id).contains(&p) {
            return Err(Error::Other("move would create a cycle".into()));
        }
        snap.entities.get(&p).ok_or(Error::NoSuchEntity(p))?;
    }
    let old_parent = rec.parent;
    let mut next = snap.clone();
    next.reparent(id, new_parent, ordinal)?;
    if let Some(p) = new_parent {
        let file = next.entities[&p].file.clone();
        next.set_file(id, file, ordinal.unwrap_or(rec.ordinal))?;
    }
    if old_parent == new_parent {
        if let Some(p) = new_parent {
            let mut kids = child_ids_of(store, &next, p)?;
            kids.retain(|c| *c != id);
            let at = ordinal.unwrap_or(kids.len() as u32) as usize;
            kids.insert(at.min(kids.len()), id);
            apply_child_holes(store, &mut next, p, &kids)?;
        }
        return Ok(next);
    }
    if let Some(p) = old_parent {
        if next.entities.contains_key(&p) {
            let mut kids = child_ids_of(store, snap, p)?;
            kids.retain(|c| *c != id);
            apply_child_holes(store, &mut next, p, &kids)?;
        }
    }
    if let Some(p) = new_parent {
        let mut kids = child_ids_of(store, &next, p)?;
        kids.retain(|c| *c != id);
        let at = ordinal.unwrap_or(kids.len() as u32) as usize;
        kids.insert(at.min(kids.len()), id);
        apply_child_holes(store, &mut next, p, &kids)?;
    }
    reresolve_subtree(store, langs, &mut next, id)?;
    Ok(next)
}

pub fn extract_hoist(
    store: &dyn Store,
    langs: &Langs,
    snap: &Snapshot,
    id: EntityId,
    new_parent: Option<EntityId>,
    ordinal: u32,
) -> Result<Snapshot> {
    move_def(store, langs, snap, id, new_parent, Some(ordinal))
}

/// Parse the destination file so inherited generics / outer locals match the
/// new parent, then copy the subtree's content and bytes back. Reorder under
/// the same parent does not call this — the env did not change.
fn reresolve_subtree(
    store: &dyn Store,
    langs: &Langs,
    snap: &mut Snapshot,
    id: EntityId,
) -> Result<()> {
    let rec = snap
        .entities
        .get(&id)
        .ok_or(Error::NoSuchEntity(id))?
        .clone();
    let lang = langs
        .for_path(&rec.file)
        .ok_or_else(|| Error::NoLanguage(rec.file.clone()))?;
    let rendered = render(snap, store, langs, false)?;
    let src = rendered
        .files
        .get(&rec.file)
        .ok_or_else(|| Error::Other(format!("re-resolve missing file {}", rec.file)))?;
    let env = env_from_snapshot(snap);
    let part = ingest_file_prev(
        src,
        rec.file.clone(),
        lang,
        store,
        snap.change,
        Some(snap),
        &env,
    )?;
    for eid in subtree(snap, id) {
        let (name, kind, parent) = {
            let rec = snap.entities.get(&eid).ok_or(Error::NoSuchEntity(eid))?;
            (rec.name.clone(), rec.kind, rec.parent)
        };
        let fresh = part
            .entities
            .get(&eid)
            .or_else(|| {
                part.entities
                    .values()
                    .find(|r| r.name == name && r.kind == kind && r.parent == parent)
            })
            .ok_or_else(|| Error::Other(format!("re-resolve lost {name}")))?;
        let content = fresh.content;
        let bytes = fresh.bytes;
        if let Some(dest) = snap.entities.get_mut(&eid) {
            dest.content = content;
            dest.bytes = bytes;
        }
    }
    Ok(())
}

pub fn delete(snap: &Snapshot, store: &dyn Store, id: EntityId) -> Result<Snapshot> {
    let tree = subtree(snap, id);
    let mut outside = Vec::new();
    for d in &tree {
        outside.extend(
            referrers(snap, store, *d)?
                .into_iter()
                .filter(|r| !tree.contains(r)),
        );
    }
    if !outside.is_empty() {
        return Err(Error::Other(format!(
            "delete refused: {} referrers outside the subtree",
            outside.len()
        )));
    }
    let parent = snap.entities.get(&id).and_then(|r| r.parent);
    let mut next = snap.clone();
    for d in tree {
        next.entities.remove(&d);
    }
    if let Some(p) = parent {
        if next.entities.contains_key(&p) {
            let mut kids = child_ids_of(store, snap, p)?;
            kids.retain(|c| *c != id);
            apply_child_holes(store, &mut next, p, &kids)?;
        }
    }
    Ok(next)
}

pub fn inline(store: &dyn Store, langs: &Langs, snap: &Snapshot, id: EntityId) -> Result<Snapshot> {
    snap.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
    let sites = name_use_sites(snap, store, id)?;
    if sites.len() != 1 {
        return Err(Error::Other(format!(
            "inline requires a single use; found {}",
            sites.len()
        )));
    }
    let referrer = sites[0];
    if subtree(snap, referrer).contains(&id) {
        return Err(Error::Other(
            "inline cannot target an item still nested in the call site; extract first".into(),
        ));
    }
    let replacement = inlined_call_text(store, langs, snap, id, referrer)?;
    let (next, _) = edit_def(store, langs, snap, referrer, &replacement)?;
    delete(&next, store, id)
}

/// `Chunk::Name(id)` sites, including recursive uses inside `id` itself.
/// The declaration hole is `Name(SELF)`, not `Name(id)`.
fn name_use_sites(snap: &Snapshot, store: &dyn Store, id: EntityId) -> Result<Vec<EntityId>> {
    let mut sites = Vec::new();
    for (oid, rec) in &snap.entities {
        let n = store
            .get_bytes_blob(rec.bytes)?
            .chunks()
            .iter()
            .filter(|c| matches!(c, Chunk::Name(e) if *e == id))
            .count();
        for _ in 0..n {
            sites.push(*oid);
        }
    }
    Ok(sites)
}

fn inlined_call_text(
    store: &dyn Store,
    langs: &Langs,
    snap: &Snapshot,
    id: EntityId,
    referrer: EntityId,
) -> Result<Vec<u8>> {
    let rec = snap.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
    let lang = langs
        .for_path(&rec.file)
        .ok_or_else(|| Error::NoLanguage(rec.file.clone()))?;
    let (callee, _) = render_entity(snap, store, id, false)?;
    let (params, body) = callee_params_and_body(lang, &callee)?;
    let (caller, map) = render_entity(snap, store, referrer, true)?;
    let ranges: Vec<ByteRange> = map
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|(r, ident)| match ident {
            IdentRef::Entity(e) if *e == id => Some(*r),
            _ => None,
        })
        .collect();
    match ranges.as_slice() {
        [range] => {
            let name_end = range.end as usize;
            if name_end > caller.len() || (range.start as usize) > name_end {
                return Err(Error::Other("inline call range is out of bounds".into()));
            }
            let (call_end, args) = extend_call(&caller, name_end)?;
            if args.len() != params.len() {
                return Err(Error::Other(format!(
                    "inline expected {} argument(s), found {}",
                    params.len(),
                    args.len()
                )));
            }
            let mut block = bind_and_body(&params, &args, &body);
            if rust_postfix(&caller, call_end) {
                if lang.name() != "rust" {
                    return Err(Error::Other(
                        "inline of an expression-position call is only supported in rust".into(),
                    ));
                }
                block = format!("({block})");
            }
            let start = range.start as usize;
            let mut out = Vec::with_capacity(caller.len() + block.len());
            out.extend_from_slice(&caller[..start]);
            out.extend_from_slice(block.as_bytes());
            out.extend_from_slice(&caller[call_end..]);
            Ok(out)
        }
        _ => Err(Error::Other(format!(
            "inline needs one call name in the unique user; found {}",
            ranges.len()
        ))),
    }
}

fn callee_params_and_body(lang: &dyn Lang, src: &[u8]) -> Result<(Vec<String>, String)> {
    let tree = parse(src, lang)?;
    let item = entity_item_node(tree.root_node(), lang)
        .ok_or_else(|| Error::Other("inline target does not parse as a language item".into()))?;
    let rule = lang
        .entity_kinds()
        .iter()
        .find(|r| r.node_kind == item.kind())
        .ok_or_else(|| Error::Other("inline target is not a language item".into()))?;
    let body_field = rule.body_field.ok_or_else(|| {
        Error::Other("inline needs an item with a body (not a type or signature)".into())
    })?;
    let body = item
        .child_by_field_name(body_field)
        .ok_or_else(|| Error::Other("inline target has no body".into()))?;
    let params = match item.child_by_field_name("parameters") {
        Some(n) => param_names(n, src)?,
        None => Vec::new(),
    };
    Ok((params, node_src(src, body)))
}

fn entity_item_node<'a>(
    node: tree_sitter::Node<'a>,
    lang: &dyn Lang,
) -> Option<tree_sitter::Node<'a>> {
    if lang
        .entity_kinds()
        .iter()
        .any(|r| r.node_kind == node.kind())
    {
        return Some(node);
    }
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        if let Some(hit) = entity_item_node(ch, lang) {
            return Some(hit);
        }
    }
    None
}

fn param_names(params: tree_sitter::Node<'_>, src: &[u8]) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut c = params.walk();
    for ch in params.named_children(&mut c) {
        match ch.kind() {
            "self_parameter" => {
                return Err(Error::Other(
                    "inline of a method (self) is not supported".into(),
                ));
            }
            "identifier" => names.push(node_src(src, ch)),
            "parameter" | "required_parameter" | "optional_parameter" | "rest_parameter"
            | "assignment_pattern" => {
                let pat = ch
                    .child_by_field_name("pattern")
                    .or_else(|| ch.child_by_field_name("name"))
                    .unwrap_or(ch);
                match simple_pat_name(pat, src) {
                    Some(n) => names.push(n),
                    None => {
                        return Err(Error::Other(
                            "inline needs simple identifier parameters".into(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(names)
}

fn simple_pat_name(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_src(src, node)),
        "mut_pattern" => {
            let mut c = node.walk();
            node.named_children(&mut c)
                .find(|ch| ch.kind() == "identifier")
                .map(|id| node_src(src, id))
        }
        _ => None,
    }
}

fn node_src(src: &[u8], n: tree_sitter::Node<'_>) -> String {
    let a = n.start_byte().min(src.len());
    let b = n.end_byte().min(src.len()).max(a);
    String::from_utf8_lossy(&src[a..b]).into_owned()
}

fn extend_call(src: &[u8], name_end: usize) -> Result<(usize, Vec<String>)> {
    let mut i = name_end;
    while i < src.len() && src[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= src.len() || src[i] != b'(' {
        return Err(Error::Other("inline needs a call site".into()));
    }
    let close = matching_paren(src, i)?;
    Ok((close + 1, split_args(&src[i + 1..close])))
}

fn matching_paren(src: &[u8], open: usize) -> Result<usize> {
    let mut i = open;
    let mut depth = 0i32;
    while i < src.len() {
        let b = src[i];
        if b == b'/' && i + 1 < src.len() {
            if src[i + 1] == b'/' {
                i += 2;
                while i < src.len() && src[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if src[i + 1] == b'*' {
                i += 2;
                while i + 1 < src.len() && !(src[i] == b'*' && src[i + 1] == b'/') {
                    i += 1;
                }
                i = i.saturating_add(2);
                continue;
            }
        }
        if b == b'"' || b == b'\'' {
            let quote = b;
            i += 1;
            while i < src.len() {
                if src[i] == b'\\' {
                    i = i.saturating_add(2);
                    continue;
                }
                if src[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                return Ok(i);
            }
        }
        i += 1;
    }
    Err(Error::Other("inline call is unclosed".into()))
}

fn split_args(inner: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < inner.len() {
        let b = inner[i];
        if b == b'"' || b == b'\'' {
            let quote = b;
            i += 1;
            while i < inner.len() {
                if inner[i] == b'\\' {
                    i = i.saturating_add(2);
                    continue;
                }
                if inner[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                push_arg(&mut out, &inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    push_arg(&mut out, &inner[start..]);
    out
}

fn push_arg(out: &mut Vec<String>, piece: &[u8]) {
    let s = String::from_utf8_lossy(piece).trim().to_string();
    if !s.is_empty() {
        out.push(s);
    }
}

fn bind_and_body(params: &[String], args: &[String], body: &str) -> String {
    if params.is_empty() {
        return body.to_string();
    }
    let mut s = String::from("{\n");
    for (p, a) in params.iter().zip(args) {
        s.push_str("    let ");
        s.push_str(p);
        s.push_str(" = ");
        s.push_str(a);
        s.push_str(";\n");
    }
    s.push_str(body);
    if !body.ends_with('\n') {
        s.push('\n');
    }
    s.push('}');
    s
}

fn rust_postfix(src: &[u8], after_call: usize) -> bool {
    let mut i = after_call;
    while i < src.len() && src[i].is_ascii_whitespace() {
        i += 1;
    }
    i < src.len() && matches!(src[i], b'?' | b'.')
}

pub fn edit_def(
    store: &dyn Store,
    langs: &Langs,
    snap: &Snapshot,
    id: EntityId,
    definition: &[u8],
) -> Result<(Snapshot, ObservedClass)> {
    let rec = snap
        .entities
        .get(&id)
        .ok_or(Error::NoSuchEntity(id))?
        .clone();
    let lang = langs
        .for_path(&rec.file)
        .ok_or_else(|| Error::NoLanguage(rec.file.clone()))?;
    let definition = item_text(store, snap, id, definition)?;
    let (root_old, part) = ingest_item_tree(
        "edit-def",
        store,
        snap,
        &rec.file,
        lang,
        rec.parent,
        &definition,
    )?;
    let mapped = remap_tree(store, part.entities, root_old, id, Some(snap))?;
    let new_rec = mapped
        .get(&id)
        .cloned()
        .ok_or_else(|| Error::Other("edit-def lost the root item".into()))?;
    if new_rec.name != rec.name {
        return Err(Error::Other(format!(
            "edit-def cannot rename {} to {}; use svc rename",
            rec.name, new_rec.name
        )));
    }
    if new_rec.kind != rec.kind {
        return Err(Error::Other(format!(
            "edit-def cannot change {} from {:?} to {:?}; delete and add-def instead",
            rec.name, rec.kind, new_rec.kind
        )));
    }
    let new_content = new_rec.content;
    let new_bytes = new_rec.bytes;
    let mut next = snap.clone();
    for cid in subtree(snap, id) {
        if cid != id {
            next.entities.remove(&cid);
        }
    }
    if let Some(dest) = next.entities.get_mut(&id) {
        dest.content = new_content;
        dest.bytes = new_bytes;
    }
    for (cid, mut child) in mapped {
        if cid == id {
            continue;
        }
        child.file = rec.file.clone();
        next.insert(cid, child)?;
    }
    let old_c = store.get_content(rec.content)?;
    let new_c = store.get_content(new_content)?;
    let (old_r, old_m) = render_entity(snap, store, id, true)?;
    let (new_r, new_m) = render_entity(&next, store, id, true)?;
    let class = classify(
        &old_c,
        &new_c,
        rec.bytes,
        new_bytes,
        &old_r,
        &new_r,
        &Default::default(),
        &Default::default(),
        old_m.as_deref().unwrap_or(&[]),
        new_m.as_deref().unwrap_or(&[]),
    );
    Ok((next, class))
}

pub fn add_def(
    store: &dyn Store,
    langs: &Langs,
    snap: &Snapshot,
    id: EntityId,
    parent: Option<EntityId>,
    ordinal: u32,
    definition: &[u8],
    intent: Intent,
) -> Result<Snapshot> {
    add_def_at(
        store, langs, snap, id, parent, None, ordinal, definition, intent,
    )
}

/// Parent's file, else the first tracked source file. An explicit `file` wins.
pub fn resolve_add_def_file(
    snap: &Snapshot,
    langs: &Langs,
    parent: Option<EntityId>,
    file: Option<RelPath>,
) -> Result<RelPath> {
    if let Some(file) = file {
        return Ok(file);
    }
    match parent {
        Some(p) => Ok(snap
            .entities
            .get(&p)
            .ok_or(Error::NoSuchEntity(p))?
            .file
            .clone()),
        None => Ok(snap
            .files
            .keys()
            .find(|p| langs.for_path(p).is_some())
            .cloned()
            .ok_or_else(|| Error::Other("no tracked source file to add into".into()))?),
    }
}

pub fn add_def_at(
    store: &dyn Store,
    langs: &Langs,
    snap: &Snapshot,
    id: EntityId,
    parent: Option<EntityId>,
    file: Option<RelPath>,
    ordinal: u32,
    definition: &[u8],
    _intent: Intent,
) -> Result<Snapshot> {
    let file = resolve_add_def_file(snap, langs, parent, file)?;
    let lang = langs
        .for_path(&file)
        .ok_or_else(|| Error::NoLanguage(file.clone()))?;
    let indent = parent
        .map(|p| sibling_indent(store, snap, p))
        .unwrap_or_default();
    let definition = add_def_text(parent, &indent, definition);
    let (root_old, part) =
        ingest_item_tree("add-def", store, snap, &file, lang, parent, &definition)?;
    let mapped = remap_tree(store, part.entities, root_old, id, None)?;
    let mut next = snap.clone();
    next.ensure_file(file.clone());
    let mut root = mapped
        .get(&id)
        .cloned()
        .ok_or_else(|| Error::Other("add-def lost the root item".into()))?;
    root.parent = parent;
    root.file = file.clone();
    root.ordinal = ordinal;
    next.insert(id, root)?;
    for (cid, mut rec) in mapped {
        if cid == id {
            continue;
        }
        rec.file = file.clone();
        next.insert(cid, rec)?;
    }
    if let Some(p) = parent {
        let mut kids = child_ids_of(store, &next, p)?;
        kids.retain(|c| *c != id);
        let at = (ordinal as usize).min(kids.len());
        kids.insert(at, id);
        apply_child_holes(store, &mut next, p, &kids)?;
    }
    Ok(next)
}

pub fn classify_def(
    store: &dyn Store,
    langs: &Langs,
    snap: &Snapshot,
    id: EntityId,
    definition: &[u8],
) -> Result<ObservedClass> {
    let (_, class) = edit_def(store, langs, snap, id, definition)?;
    Ok(class)
}

pub fn show(store: &dyn Store, snap: &Snapshot, id: EntityId) -> Result<String> {
    let rec = snap.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
    let content = store.get_content(rec.content)?;
    Ok(format_tokens(snap, &content.tokens, &rec.name))
}

pub fn format_tokens(snap: &Snapshot, tokens: &[Token], self_name: &str) -> String {
    let mut out = String::new();
    for t in tokens {
        match t {
            Token::Punct(p) | Token::Kw(p) | Token::Lit(p) => out.push_str(p),
            Token::Binder(s, _) => out.push_str(&format!("${}", s.0)),
            Token::Ident(IdentRef::Local(s, _)) => out.push_str(&format!("${}", s.0)),
            Token::Ident(IdentRef::Free(n)) => out.push_str(n),
            Token::Ident(IdentRef::Entity(eid)) if *eid == EntityId::SELF => {
                out.push_str(self_name)
            }
            Token::Ident(IdentRef::Entity(eid)) | Token::Child(eid) => {
                let name = snap
                    .entities
                    .get(eid)
                    .map(|e| e.name.as_str())
                    .unwrap_or("?");
                out.push_str(&format!("#{name}⟨{}⟩", eid.short()));
            }
        }
        out.push(' ');
    }
    out
}

pub fn status_report(prev: &Snapshot, next: &Snapshot) -> StatusReport {
    let deltas = super::diff(prev, next);
    let mut layout = 0usize;
    let mut semantic = 0usize;
    for d in &deltas {
        match d {
            Delta::Edited(_, ObservedClass::Alpha | ObservedClass::DocsOnly) => layout += 1,
            Delta::Relocated { .. } => layout += 1,
            _ => semantic += 1,
        }
    }
    StatusReport {
        entities: next.entities.len(),
        deltas,
        layout,
        semantic,
    }
}

#[derive(Clone, Debug)]
pub struct StatusReport {
    pub entities: usize,
    pub deltas: Vec<Delta>,
    pub layout: usize,
    pub semantic: usize,
}

impl StatusReport {
    pub fn summary(&self) -> String {
        if self.deltas.is_empty() {
            format!("{} entities, 0 changes", self.entities)
        } else if self.semantic == 0 {
            format!("no semantic changes; {} layout", self.layout)
        } else {
            format!(
                "{} semantic; {} layout ({} entities)",
                self.semantic, self.layout, self.entities
            )
        }
    }
}

pub fn snapshot_working_copy(
    store: &dyn Store,
    langs: &Langs,
    files: &BTreeMap<RelPath, Vec<u8>>,
    prev: Option<&Snapshot>,
    change: ChangeId,
) -> Result<Snapshot> {
    snapshot_files(store, langs, files, prev, change)
}

/// Entities whose content or bytes *name* `id`. Child holes are containment,
/// not uses — counting them made every nested delete refuse.
pub fn referrers(snap: &Snapshot, store: &dyn Store, id: EntityId) -> Result<Vec<EntityId>> {
    let mut out = Vec::new();
    for (oid, rec) in &snap.entities {
        if *oid == id {
            continue;
        }
        let in_content = store
            .get_content(rec.content)?
            .tokens
            .iter()
            .any(|t| match t {
                Token::Ident(IdentRef::Entity(e)) => *e == id,
                _ => false,
            });
        let in_bytes = || -> Result<bool> {
            Ok(store
                .get_bytes_blob(rec.bytes)?
                .chunks()
                .iter()
                .any(|c| match c {
                    Chunk::Name(e) => *e == id,
                    _ => false,
                }))
        };
        if in_content || in_bytes()? {
            out.push(*oid);
        }
    }
    Ok(out)
}

fn child_ids_in_chunks(chunks: &[Chunk]) -> Vec<EntityId> {
    chunks
        .iter()
        .filter_map(|c| match c {
            Chunk::Child(id) => Some(*id),
            _ => None,
        })
        .collect()
}

fn child_ids_of(store: &dyn Store, snap: &Snapshot, parent: EntityId) -> Result<Vec<EntityId>> {
    let rec = snap
        .entities
        .get(&parent)
        .ok_or(Error::NoSuchEntity(parent))?;
    Ok(child_ids_in_chunks(
        store.get_bytes_blob(rec.bytes)?.chunks(),
    ))
}

fn apply_child_holes(
    store: &dyn Store,
    snap: &mut Snapshot,
    parent: EntityId,
    want: &[EntityId],
) -> Result<()> {
    let rec = snap
        .entities
        .get(&parent)
        .ok_or(Error::NoSuchEntity(parent))?
        .clone();
    let bytes = store.get_bytes_blob(rec.bytes)?;
    let content = store.get_content(rec.content)?;
    let new_bytes = rewrite_bytes_children(&bytes, want)?;
    let new_content = Content {
        tokens: rewrite_content_children(content.tokens, want),
    };
    let bytes_id = store.put_bytes_blob(&new_bytes)?;
    let content_id = store.put_content(&new_content)?;
    let rec = snap.entities.get_mut(&parent).unwrap();
    rec.bytes = bytes_id;
    rec.content = content_id;
    Ok(())
}

fn rewrite_bytes_children(bytes: &Bytes, want: &[EntityId]) -> Result<Bytes> {
    let have = child_ids_in_chunks(bytes.chunks());
    if have == want {
        return Ok(Bytes::new(
            bytes.src().to_vec(),
            bytes.chunks().to_vec(),
            bytes.local_ranges().to_vec(),
        )?);
    }
    let want_set: BTreeSet<EntityId> = want.iter().copied().collect();
    let mut src = bytes.src().to_vec();
    let mut chunks: Vec<Chunk> = bytes
        .chunks()
        .iter()
        .cloned()
        .filter(|c| match c {
            Chunk::Child(id) => want_set.contains(id),
            _ => true,
        })
        .collect();
    for (i, id) in want.iter().enumerate() {
        if child_ids_in_chunks(&chunks).contains(id) {
            continue;
        }
        chunks = insert_child_chunk(&mut src, chunks, *id, i);
    }
    let have = child_ids_in_chunks(&chunks);
    if have != want {
        let mut iter = want.iter();
        for c in &mut chunks {
            if let Chunk::Child(id) = c
                && let Some(next) = iter.next()
            {
                *id = *next;
            }
        }
    }
    Bytes::new(src, chunks, bytes.local_ranges().to_vec())
}

fn insert_child_chunk(
    src: &mut Vec<u8>,
    mut chunks: Vec<Chunk>,
    id: EntityId,
    ordinal: usize,
) -> Vec<Chunk> {
    let positions: Vec<usize> = chunks
        .iter()
        .enumerate()
        .filter_map(|(i, c)| matches!(c, Chunk::Child(_)).then_some(i))
        .collect();
    let insert_at = if ordinal < positions.len() {
        positions[ordinal]
    } else if let Some(&last) = positions.last() {
        last + 1
    } else {
        split_close_brace(src, &mut chunks)
    };
    // A member's bytes end at its last token; whatever follows must begin on a new
    // line. The next sibling carries its own leading newline, and so does the parent's
    // closing text in every rendered file — only a bare `}` (split_close_brace) or a
    // literal that starts mid-line needs one added here.
    let next_starts_on_new_line = match chunks.get(insert_at) {
        Some(Chunk::Child(_)) => true,
        Some(Chunk::Literal(r)) => src.get(r.start as usize) == Some(&b'\n'),
        Some(Chunk::Name(_)) | None => false,
    };
    chunks.insert(insert_at, Chunk::Child(id));
    if !next_starts_on_new_line {
        let extra_start = src.len() as u32;
        src.extend_from_slice(b"\n");
        let extra = ByteRange {
            start: extra_start,
            end: src.len() as u32,
        };
        chunks.insert(insert_at + 1, Chunk::Literal(extra));
    }
    chunks
}

fn split_close_brace(src: &[u8], chunks: &mut Vec<Chunk>) -> usize {
    for i in (0..chunks.len()).rev() {
        let Chunk::Literal(r) = chunks[i] else {
            continue;
        };
        let slice = &src[r.start as usize..r.end as usize];
        let Some(rel) = slice.iter().rposition(|&b| b == b'}') else {
            continue;
        };
        let brace = r.start + rel as u32;
        chunks.remove(i);
        let mut at = i;
        if brace > r.start {
            chunks.insert(
                at,
                Chunk::Literal(ByteRange {
                    start: r.start,
                    end: brace,
                }),
            );
            at += 1;
        }
        chunks.insert(
            at,
            Chunk::Literal(ByteRange {
                start: brace,
                end: r.end,
            }),
        );
        return at;
    }
    chunks.len()
}

fn rewrite_content_children(tokens: Vec<Token>, want: &[EntityId]) -> Vec<Token> {
    let content_kids = |tokens: &[Token]| -> Vec<EntityId> {
        tokens
            .iter()
            .filter_map(|t| match t {
                Token::Child(id) => Some(*id),
                _ => None,
            })
            .collect()
    };
    if content_kids(&tokens) == want {
        return tokens;
    }
    let want_set: BTreeSet<EntityId> = want.iter().copied().collect();
    let mut tokens: Vec<Token> = tokens
        .into_iter()
        .filter(|t| match t {
            Token::Child(id) => want_set.contains(id),
            _ => true,
        })
        .collect();
    for (i, id) in want.iter().enumerate() {
        if content_kids(&tokens).contains(id) {
            continue;
        }
        let positions: Vec<usize> = tokens
            .iter()
            .enumerate()
            .filter_map(|(j, t)| matches!(t, Token::Child(_)).then_some(j))
            .collect();
        let at = if i < positions.len() {
            positions[i]
        } else if let Some(&last) = positions.last() {
            last + 1
        } else {
            tokens
                .iter()
                .rposition(|t| matches!(t, Token::Punct(p) if p.as_ref() == "}"))
                .unwrap_or(tokens.len())
        };
        tokens.insert(at, Token::Child(*id));
    }
    if content_kids(&tokens) != want {
        let mut iter = want.iter();
        for t in &mut tokens {
            if let Token::Child(id) = t
                && let Some(next) = iter.next()
            {
                *id = *next;
            }
        }
    }
    tokens
}

fn subtree(snap: &Snapshot, id: EntityId) -> BTreeSet<EntityId> {
    let mut out = BTreeSet::from([id]);
    let mut grow = true;
    while grow {
        grow = false;
        for (cid, rec) in &snap.entities {
            if let Some(p) = rec.parent
                && out.contains(&p)
                && out.insert(*cid)
            {
                grow = true;
            }
        }
    }
    out
}

pub fn commit_snapshot(store: &dyn Store, snap: &Snapshot) -> Result<crate::ids::SnapshotId> {
    let id = store.put_snapshot(snap)?;
    store.set_root(id)?;
    store.set_head(snap.change, id)?;
    Ok(id)
}

pub fn rust_langs() -> Langs {
    Langs::new(vec![Box::new(crate::RustLang)])
}

/// Same nested-child keep as `edit_def`. Returns the new snapshot plus the
/// edited root's hashes so O3 can assert callee holes without walking entities.
pub fn redefine(
    store: &dyn Store,
    langs: &Langs,
    snapshot: &Snapshot,
    id: EntityId,
    text: &[u8],
) -> Result<(Snapshot, crate::ids::ContentId, crate::ids::BytesId)> {
    let (next, _) = edit_def(store, langs, snapshot, id, text)?;
    let rec = next.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
    let hashes = (rec.content, rec.bytes);
    Ok((next, hashes.0, hashes.1))
}

/// The one item a verb body must be: parses without error nodes, exactly one root
/// entity. Ingest itself is lenient (a checked-in file may be mid-edit); the typed
/// write path is not.
/// Parse and ingest one definition. Under a parent whose members do not parse on their
/// own (a JS class), the text is wrapped in the language's member shell and the member,
/// not the shell, is returned as the root.
fn ingest_item_tree(
    verb: &str,
    store: &dyn Store,
    snap: &Snapshot,
    file: &RelPath,
    lang: &dyn crate::lang::Lang,
    parent: Option<EntityId>,
    text: &[u8],
) -> Result<(EntityId, Snapshot)> {
    let parent_kind = parent.and_then(|p| snap.entities.get(&p)).map(|r| r.kind);
    let shell = parent_kind.and_then(|k| lang.member_shell(k));
    let wrapped: Vec<u8> = match shell {
        Some((open, close)) => [open.as_bytes(), text, close.as_bytes()].concat(),
        None => text.to_vec(),
    };
    if super::parse(&wrapped, lang)?.root_node().has_error() {
        return Err(Error::Parse(format!(
            "{verb} definition does not parse as {}",
            lang.name()
        )));
    }
    let mut env = env_from_snapshot(snap);
    fill_self_methods_from_snapshot(&mut env, snap, parent);
    let mut part = ingest_file_with_env(&wrapped, file.clone(), lang, store, snap.change, &env)?;
    let roots: Vec<EntityId> = part
        .entities
        .iter()
        .filter(|(_, r)| r.parent.is_none())
        .map(|(id, _)| *id)
        .collect();
    let [root] = roots.as_slice() else {
        return Err(Error::Other(format!(
            "{verb} definition must parse to exactly one item"
        )));
    };
    let root = *root;
    if shell.is_none() {
        return Ok((root, part));
    }
    let members: Vec<EntityId> = part
        .entities
        .iter()
        .filter(|(_, r)| r.parent == Some(root))
        .map(|(id, _)| *id)
        .collect();
    let [member] = members.as_slice() else {
        return Err(Error::Other(format!(
            "{verb} definition must be exactly one member"
        )));
    };
    let member = *member;
    part.entities.remove(&root);
    part.entities.get_mut(&member).unwrap().parent = None;
    Ok((member, part))
}

fn derived_id(parent: EntityId, rec: &EntityRecord) -> EntityId {
    let mut h = blake3::Hasher::new();
    h.update(parent.0.as_bytes());
    h.update(&[0]);
    h.update(format!("{:?}", rec.kind).as_bytes());
    h.update(&[0]);
    h.update(rec.name.as_bytes());
    h.update(&[0]);
    h.update(&rec.ordinal.to_le_bytes());
    let hash = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    EntityId(uuid::Uuid::from_bytes(bytes))
}

fn remap_tree(
    store: &dyn Store,
    tree: BTreeMap<EntityId, EntityRecord>,
    root_old: EntityId,
    root_new: EntityId,
    prev: Option<&Snapshot>,
) -> Result<BTreeMap<EntityId, EntityRecord>> {
    let mut id_map = BTreeMap::new();
    id_map.insert(root_old, root_new);
    let mut taken = BTreeSet::from([root_new]);
    let mut queue = vec![root_old];
    while let Some(old) = queue.pop() {
        let parent_new = id_map[&old];
        let mut kids: Vec<(EntityId, EntityRecord)> = tree
            .iter()
            .filter(|(_, r)| r.parent == Some(old))
            .map(|(i, r)| (*i, r.clone()))
            .collect();
        kids.sort_by_key(|(_, r)| r.ordinal);
        for (old_id, rec) in kids {
            let new_id = match prev {
                Some(prev) => {
                    let key = SigKey::new(Some(parent_new), &rec.file, rec.kind, rec.name.clone());
                    prev.entities
                        .iter()
                        .find_map(|(pid, r)| {
                            (!taken.contains(pid) && r.sig_key() == key).then_some(*pid)
                        })
                        .unwrap_or_else(|| derived_id(parent_new, &rec))
                }
                None => derived_id(parent_new, &rec),
            };
            taken.insert(new_id);
            id_map.insert(old_id, new_id);
            queue.push(old_id);
        }
    }
    let mut out = BTreeMap::new();
    for (old, rec) in &tree {
        let new_id = *id_map.get(old).unwrap_or(old);
        let mut rec = rec.clone();
        rec.parent = rec.parent.map(|p| *id_map.get(&p).unwrap_or(&p));
        let bytes = remap_bytes(&store.get_bytes_blob(rec.bytes)?, &id_map)?;
        let content = Content {
            tokens: remap_tokens(store.get_content(rec.content)?.tokens, &id_map),
        };
        rec.bytes = store.put_bytes_blob(&bytes)?;
        rec.content = store.put_content(&content)?;
        out.insert(new_id, rec);
    }
    Ok(out)
}

fn remap_id(id: EntityId, map: &BTreeMap<EntityId, EntityId>) -> EntityId {
    if id == EntityId::SELF {
        id
    } else {
        *map.get(&id).unwrap_or(&id)
    }
}

fn remap_bytes(bytes: &Bytes, map: &BTreeMap<EntityId, EntityId>) -> Result<Bytes> {
    let chunks: Vec<Chunk> = bytes
        .chunks()
        .iter()
        .map(|c| match c {
            Chunk::Child(id) => Chunk::Child(remap_id(*id, map)),
            Chunk::Name(id) => Chunk::Name(remap_id(*id, map)),
            other => other.clone(),
        })
        .collect();
    let locals: Vec<(ByteRange, IdentRef)> = bytes
        .local_ranges()
        .iter()
        .map(|(r, ident)| {
            let ident = match ident {
                IdentRef::Entity(id) => IdentRef::Entity(remap_id(*id, map)),
                other => other.clone(),
            };
            (*r, ident)
        })
        .collect();
    Bytes::new(bytes.src().to_vec(), chunks, locals)
}

fn remap_tokens(tokens: Vec<Token>, map: &BTreeMap<EntityId, EntityId>) -> Vec<Token> {
    tokens
        .into_iter()
        .map(|t| match t {
            Token::Child(id) => Token::Child(remap_id(id, map)),
            Token::Ident(IdentRef::Entity(id)) => Token::Ident(IdentRef::Entity(remap_id(id, map))),
            other => other,
        })
        .collect()
}

/// Keep the entity's leading trivia (blank lines / docs attached by extent)
/// and tolerate a missing trailing newline (source_file vs item range tie).
fn item_text(store: &dyn Store, snap: &Snapshot, id: EntityId, text: &[u8]) -> Result<Vec<u8>> {
    let (old, _) = super::render_entity(snap, store, id, false)?;
    let lead = old.iter().take_while(|b| b.is_ascii_whitespace()).count();
    let mut out = Vec::new();
    if !text.first().is_some_and(|b| b.is_ascii_whitespace()) {
        out.extend_from_slice(&old[..lead]);
    }
    out.extend_from_slice(text);
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    Ok(out)
}

/// Indentation of `parent`'s existing members (the whitespace after the last newline
/// before the first member's text); four spaces when it has none yet.
fn sibling_indent(store: &dyn Store, snap: &Snapshot, parent: EntityId) -> Vec<u8> {
    let first = snap
        .entities
        .iter()
        .filter(|(_, r)| r.parent == Some(parent))
        .min_by_key(|(_, r)| r.ordinal)
        .map(|(id, _)| *id);
    let indent = first
        .and_then(|id| render_entity(snap, store, id, false).ok())
        .and_then(|(bytes, _)| {
            let lead = bytes.iter().take_while(|b| b.is_ascii_whitespace()).count();
            let last_nl = bytes[..lead].iter().rposition(|b| *b == b'\n')?;
            Some(bytes[last_nl + 1..lead].to_vec())
        });
    indent
        .filter(|i| !i.is_empty())
        .unwrap_or_else(|| b"    ".to_vec())
}

/// A new item needs a blank line before it or render glues `}fn` / `;fn`. Nested items
/// (an impl method, a class member) are also indented one level, since the body an
/// agent passes is written at column 0.
fn add_def_text(parent: Option<EntityId>, indent: &[u8], text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let bare = !text.first().is_some_and(|b| b.is_ascii_whitespace());
    if bare {
        out.extend_from_slice(b"\n\n");
    }
    if parent.is_some() && bare {
        for (i, line) in text.split(|b| *b == b'\n').enumerate() {
            if i > 0 {
                out.push(b'\n');
            }
            if !line.is_empty() {
                out.extend_from_slice(indent);
                out.extend_from_slice(line);
            }
        }
    } else {
        out.extend_from_slice(text);
    }
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}
