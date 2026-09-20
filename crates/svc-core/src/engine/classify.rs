use crate::content::{Content, IdentRef};
use crate::delta::ObservedClass;
use crate::error::{Error, Result};
use crate::ids::{ByteRange, BytesId, EntityId};
use crate::snapshot::Snapshot;
use crate::store::Store;

use super::align::{
    SlotKey, binder_sites, equal_lines, idents_in, is_site, pair_unmapped_by_spelling,
    slot_bijection,
};
use super::render_entity;

/// One side of an edit as the classifier sees it: canonical content, the exact bytes,
/// and the rendered text with its identifier map.
pub struct Side<'a> {
    pub content: &'a Content,
    pub bytes: BytesId,
    pub render: &'a [u8],
    pub map: &'a [(ByteRange, IdentRef)],
}

/// First applicable class. Aligns rendered whole items, not neutralized tokens.
pub fn classify(old: Side<'_>, new: Side<'_>) -> ObservedClass {
    if old.content == new.content {
        if old.bytes == new.bytes {
            return ObservedClass::Alpha;
        }
        return if only_local_spellings(old.render, new.render, old.map, new.map) {
            ObservedClass::Alpha
        } else {
            ObservedClass::DocsOnly
        };
    }
    if surviving_refs_ok(old.render, new.render, old.map, new.map) {
        ObservedClass::BindingPreserving
    } else {
        ObservedClass::BindingChanging
    }
}

/// The class of `id`'s edit between two snapshots that both hold it: renders both sides
/// with their identifier maps and hands them to [`classify`]. The one way edit_def and
/// diff ask the question, so they cannot answer it differently.
pub fn classify_entity(
    store: &dyn Store,
    prev: &Snapshot,
    next: &Snapshot,
    id: EntityId,
) -> Result<ObservedClass> {
    let side = |snap: &Snapshot| -> Result<(Content, BytesId, Vec<u8>, Vec<(ByteRange, IdentRef)>)> {
        let rec = snap.entities.get(&id).ok_or(Error::NoSuchEntity(id))?;
        let (render, map) = render_entity(snap, store, id, true)?;
        Ok((store.get_content(rec.content)?, rec.bytes, render, map.unwrap_or_default()))
    };
    let (old_c, old_b, old_r, old_m) = side(prev)?;
    let (new_c, new_b, new_r, new_m) = side(next)?;
    Ok(classify(
        Side { content: &old_c, bytes: old_b, render: &old_r, map: &old_m },
        Side { content: &new_c, bytes: new_b, render: &new_r, map: &new_m },
    ))
}

fn only_local_spellings(
    old: &[u8],
    new: &[u8],
    old_map: &[(ByteRange, IdentRef)],
    new_map: &[(ByteRange, IdentRef)],
) -> bool {
    let strip = |src: &[u8], map: &[(ByteRange, IdentRef)]| {
        let mut out = src.to_vec();
        for (r, ident) in map.iter().rev() {
            if matches!(ident, IdentRef::Local(_, _)) {
                let a = r.start.min(out.len() as u32) as usize;
                let b = r.end.min(out.len() as u32) as usize;
                if a <= b {
                    out.splice(a..b, std::iter::once(b'$'));
                }
            }
        }
        out
    };
    strip(old, old_map) == strip(new, new_map)
}

fn surviving_refs_ok(
    old_render: &[u8],
    new_render: &[u8],
    old_map: &[(ByteRange, IdentRef)],
    new_map: &[(ByteRange, IdentRef)],
) -> bool {
    let pairs = equal_lines(old_render, new_render);
    let mut bijection = slot_bijection(&pairs, old_map, new_map);
    pair_unmapped_by_spelling(&mut bijection, old_render, new_render, old_map, new_map);
    let old_binders = binder_sites(old_map);
    for (o_line, n_line) in &pairs {
        let o_ids = idents_in(old_map, *o_line);
        let n_ids = idents_in(new_map, *n_line);
        if o_ids.len() != n_ids.len() {
            continue;
        }
        for ((or, o), (_, n)) in o_ids.iter().zip(n_ids.iter()) {
            if let IdentRef::Local(os, ons) = o
                && is_site(*or, (*os, *ons), &old_binders)
            {
                continue;
            }
            if !same_target(o, n, &bijection) {
                return false;
            }
        }
    }
    true
}

fn same_target(
    old: &IdentRef,
    new: &IdentRef,
    bijection: &std::collections::HashMap<SlotKey, SlotKey>,
) -> bool {
    match (old, new) {
        (IdentRef::Entity(a), IdentRef::Entity(b)) => a == b,
        (IdentRef::Free(a), IdentRef::Free(b)) => a == b,
        (IdentRef::Local(os, ons), IdentRef::Local(ns, nns)) => {
            bijection.get(&(*os, *ons)) == Some(&(*ns, *nns))
        }
        _ => false,
    }
}
