use crate::content::{Content, IdentRef};
use crate::delta::ObservedClass;
use crate::ids::{ByteRange, BytesId};
use crate::lang::Resolution;

use super::align::{binder_sites, equal_lines, idents_in, is_site, slot_bijection, SlotKey};

/// First applicable class. Aligns rendered whole items, not neutralized tokens.
pub fn classify(
    old: &Content,
    new: &Content,
    old_bytes: BytesId,
    new_bytes: BytesId,
    old_render: &[u8],
    new_render: &[u8],
    _old_res: &Resolution,
    _new_res: &Resolution,
    old_map: &[(ByteRange, IdentRef)],
    new_map: &[(ByteRange, IdentRef)],
) -> ObservedClass {
    if old == new {
        if old_bytes == new_bytes {
            return ObservedClass::Alpha;
        }
        return if only_local_spellings(old_render, new_render, old_map, new_map) {
            ObservedClass::Alpha
        } else {
            ObservedClass::DocsOnly
        };
    }
    if surviving_refs_ok(old_render, new_render, old_map, new_map) {
        ObservedClass::BindingPreserving
    } else {
        ObservedClass::BindingChanging
    }
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
    let bijection = slot_bijection(&pairs, old_map, new_map);
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

fn same_target(old: &IdentRef, new: &IdentRef, bijection: &std::collections::HashMap<SlotKey, SlotKey>) -> bool {
    match (old, new) {
        (IdentRef::Entity(a), IdentRef::Entity(b)) => a == b,
        (IdentRef::Free(a), IdentRef::Free(b)) => a == b,
        (IdentRef::Local(os, ons), IdentRef::Local(ns, nns)) => {
            bijection.get(&(*os, *ons)) == Some(&(*ns, *nns))
        }
        _ => false,
    }
}
