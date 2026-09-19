use std::collections::{HashMap, HashSet};

use similar::{ChangeTag, TextDiff};

use crate::content::{Content, IdentRef, Namespace};
use crate::delta::ObservedClass;
use crate::ids::{ByteRange, BytesId, Slot};
use crate::lang::Resolution;

/// SPEC §3.3: first applicable class. Aligns rendered whole items, not neutralized tokens.
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
    let old_s = String::from_utf8_lossy(old_render);
    let new_s = String::from_utf8_lossy(new_render);
    let old_lines = line_spans(old_render);
    let new_lines = line_spans(new_render);
    let diff = TextDiff::from_lines(old_s.as_ref(), new_s.as_ref());
    let mut old_i = 0usize;
    let mut new_i = 0usize;
    let mut pairs: Vec<(ByteRange, ByteRange)> = Vec::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let (Some(o), Some(n)) = (old_lines.get(old_i), new_lines.get(new_i)) {
                    pairs.push((*o, *n));
                }
                old_i += 1;
                new_i += 1;
            }
            ChangeTag::Delete => old_i += 1,
            ChangeTag::Insert => new_i += 1,
        }
    }

    let mut bijection: HashMap<(Slot, Namespace), (Slot, Namespace)> = HashMap::new();
    let mut seen_old: HashSet<(Slot, Namespace)> = HashSet::new();
    for (o_line, n_line) in &pairs {
        let o_ids = idents_in(old_map, *o_line);
        let n_ids = idents_in(new_map, *n_line);
        for (o, n) in o_ids.iter().zip(n_ids.iter()) {
            if let (IdentRef::Local(os, ons), IdentRef::Local(ns, nns)) = (o, n) {
                let key = (*os, *ons);
                if seen_old.insert(key) {
                    bijection.insert(key, (*ns, *nns));
                }
            }
        }
    }

    let mut seen_ref: HashSet<(Slot, Namespace)> = HashSet::new();
    for (o_line, n_line) in &pairs {
        let o_ids = idents_in(old_map, *o_line);
        let n_ids = idents_in(new_map, *n_line);
        if o_ids.len() != n_ids.len() {
            continue;
        }
        for (o, n) in o_ids.iter().zip(n_ids.iter()) {
            if let IdentRef::Local(os, ons) = o {
                if seen_ref.insert((*os, *ons)) {
                    // first aligned occurrence of this slot is the binder site
                    continue;
                }
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
    bijection: &HashMap<(Slot, Namespace), (Slot, Namespace)>,
) -> bool {
    match (old, new) {
        (IdentRef::Entity(a), IdentRef::Entity(b)) => a == b,
        (IdentRef::Free(a), IdentRef::Free(b)) => a == b,
        (IdentRef::Local(os, ons), IdentRef::Local(ns, nns)) => {
            match bijection.get(&(*os, *ons)) {
                Some((ps, pns)) => ps == ns && pns == nns,
                None => false,
            }
        }
        _ => false,
    }
}

fn idents_in(map: &[(ByteRange, IdentRef)], line: ByteRange) -> Vec<IdentRef> {
    let mut hits: Vec<_> = map
        .iter()
        .filter(|(r, _)| r.start >= line.start && r.end <= line.end)
        .cloned()
        .collect();
    hits.sort_by_key(|(r, _)| r.start);
    hits.into_iter().map(|(_, i)| i).collect()
}

fn line_spans(src: &[u8]) -> Vec<ByteRange> {
    let mut out = Vec::new();
    let mut start = 0u32;
    for (i, b) in src.iter().enumerate() {
        if *b == b'\n' {
            let end = (i + 1) as u32;
            out.push(ByteRange { start, end });
            start = end;
        }
    }
    if start as usize <= src.len() && (start as usize) < src.len() {
        out.push(ByteRange {
            start,
            end: src.len() as u32,
        });
    } else if src.is_empty() {
        out.push(ByteRange { start: 0, end: 0 });
    }
    out
}
