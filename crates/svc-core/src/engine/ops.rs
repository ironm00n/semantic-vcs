use std::collections::{BTreeMap, BTreeSet};

use crate::content::{Chunk, IdentRef, Token};
use crate::delta::{Delta, ObservedClass};
use crate::entity::EntityRecord;
use crate::error::{Error, Result};
use crate::ids::{ChangeId, EntityId, RelPath};
use crate::lang::Langs;
use crate::op::Intent;
use crate::snapshot::Snapshot;
use crate::store::Store;

use super::{classify, env_from_snapshot, ingest_file_with_env, render_entity, snapshot_files};

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
    let p = spec.to_ascii_lowercase().replace('-', "");
    let mut hits: Vec<_> = snap
        .entities
        .keys()
        .copied()
        .filter(|id| id.short() == p || id.to_string().replace('-', "").contains(&p))
        .collect();
    hits.sort();
    hits.dedup();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(Error::NotFound(spec.into())),
        _ => Err(Error::Other(format!("ambiguous entity {spec}"))),
    }
}

/// Attribute-only: referrers keep `Chunk::Name` holes. No rehash.
pub fn rename(snap: &Snapshot, id: EntityId, new: &str) -> Result<Snapshot> {
    if !snap.entities.contains_key(&id) {
        return Err(Error::NoSuchEntity(id));
    }
    let mut next = snap.clone();
    next.entities.get_mut(&id).unwrap().name = new.to_string();
    Ok(next)
}

pub fn relocate(snap: &Snapshot, id: EntityId, file: RelPath, ordinal: u32) -> Result<Snapshot> {
    let mut next = snap.clone();
    let rec = next.entities.get_mut(&id).ok_or(Error::NoSuchEntity(id))?;
    rec.file = file;
    rec.ordinal = ordinal;
    Ok(next)
}

pub fn move_def(
    snap: &Snapshot,
    id: EntityId,
    parent: Option<EntityId>,
    ordinal: Option<u32>,
) -> Result<Snapshot> {
    let mut next = snap.clone();
    let rec = next.entities.get_mut(&id).ok_or(Error::NoSuchEntity(id))?;
    rec.parent = parent;
    if let Some(o) = ordinal {
        rec.ordinal = o;
    }
    Ok(next)
}

pub fn extract_hoist(
    snap: &Snapshot,
    id: EntityId,
    new_parent: Option<EntityId>,
    ordinal: u32,
) -> Result<Snapshot> {
    move_def(snap, id, new_parent, Some(ordinal))
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
    let mut next = snap.clone();
    for d in tree {
        next.entities.remove(&d);
    }
    Ok(next)
}

pub fn inline(snap: &Snapshot, store: &dyn Store, id: EntityId) -> Result<Snapshot> {
    let refs = referrers(snap, store, id)?;
    if refs.len() != 1 {
        return Err(Error::Other(format!(
            "inline requires a single use; found {}",
            refs.len()
        )));
    }
    delete(snap, store, id)
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
    let new_rec = ingest_one_item("edit-def", store, snap, &rec.file, lang, &definition)?;
    if new_rec.name != rec.name {
        return Err(Error::Other(format!(
            "edit-def cannot rename {} to {}; use svc rename",
            rec.name, new_rec.name
        )));
    }
    let new_content = new_rec.content;
    let new_bytes = new_rec.bytes;
    let mut next = snap.clone();
    if let Some(dest) = next.entities.get_mut(&id) {
        dest.content = new_content;
        dest.bytes = new_bytes;
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
    _intent: Intent,
) -> Result<Snapshot> {
    let file = match parent {
        Some(p) => snap
            .entities
            .get(&p)
            .ok_or(Error::NoSuchEntity(p))?
            .file
            .clone(),
        None => snap
            .files
            .keys()
            .find(|p| langs.for_path(p).is_some())
            .cloned()
            .ok_or_else(|| Error::Other("no tracked source file to add into".into()))?,
    };
    let lang = langs
        .for_path(&file)
        .ok_or_else(|| Error::NoLanguage(file.clone()))?;
    let definition = add_def_text(parent, definition);
    let mut rec = ingest_one_item("add-def", store, snap, &file, lang, &definition)?;
    let mut next = snap.clone();
    rec.parent = parent;
    rec.file = file;
    rec.ordinal = ordinal;
    next.entities.insert(id, rec);
    if !next.files.contains_key(&next.entities[&id].file) {
        next.files
            .insert(next.entities[&id].file.clone(), Default::default());
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

/// Entities whose content or bytes mention `id`. A missing blob is an error, not "no referrer".
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
                Token::Ident(IdentRef::Entity(e)) | Token::Child(e) => *e == id,
                _ => false,
            });
        let in_bytes = || -> Result<bool> {
            Ok(store
                .get_bytes_blob(rec.bytes)?
                .chunks()
                .iter()
                .any(|c| match c {
                    Chunk::Child(e) | Chunk::Name(e) => *e == id,
                    _ => false,
                }))
        };
        if in_content || in_bytes()? {
            out.push(*oid);
        }
    }
    Ok(out)
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

pub fn redefine(
    store: &dyn Store,
    langs: &Langs,
    snapshot: &Snapshot,
    id: EntityId,
    text: &[u8],
) -> Result<(crate::ids::ContentId, crate::ids::BytesId)> {
    let rec = snapshot.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
    let lang = langs
        .for_path(&rec.file)
        .ok_or_else(|| Error::NoLanguage(rec.file.clone()))?;
    let text = item_text(store, snapshot, id, text)?;
    let new_rec = ingest_one_item("redefine", store, snapshot, &rec.file, lang, &text)?;
    if new_rec.name != rec.name {
        return Err(Error::Other(format!(
            "redefine cannot rename {} to {}; use svc rename",
            rec.name, new_rec.name
        )));
    }
    Ok((new_rec.content, new_rec.bytes))
}

/// The one item a verb body must be: parses without error nodes, exactly one root
/// entity. Ingest itself is lenient (a checked-in file may be mid-edit); the typed
/// write path is not.
fn ingest_one_item(
    verb: &str,
    store: &dyn Store,
    snap: &Snapshot,
    file: &RelPath,
    lang: &dyn crate::lang::Lang,
    text: &[u8],
) -> Result<EntityRecord> {
    if super::parse(text, lang)?.root_node().has_error() {
        return Err(Error::Parse(format!(
            "{verb} definition does not parse as {}",
            lang.name()
        )));
    }
    let part = ingest_file_with_env(
        text,
        file.clone(),
        lang,
        store,
        snap.change,
        &env_from_snapshot(snap),
    )?;
    let mut roots = part.entities.into_values().filter(|r| r.parent.is_none());
    match (roots.next(), roots.next()) {
        (Some(rec), None) => Ok(rec),
        _ => Err(Error::Other(format!(
            "{verb} definition must parse to exactly one item"
        ))),
    }
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

/// File-level items need a blank line before them or render glues `}fn` / `;fn`.
fn add_def_text(parent: Option<EntityId>, text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if parent.is_none() && !text.first().is_some_and(|b| b.is_ascii_whitespace()) {
        out.extend_from_slice(b"\n\n");
    }
    out.extend_from_slice(text);
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}
