//! Line-level alignment of two renders of the same entity, and the local-slot
//! bijection it induces. Shared by the edit classifier and the merge binding
//! post-condition so both answer "is this the same binder?" the same way.
use std::collections::HashMap;

use similar::{ChangeTag, TextDiff};

use crate::content::{IdentRef, Namespace};
use crate::ids::{ByteRange, Slot};

pub type SlotKey = (Slot, Namespace);

pub fn line_spans(src: &[u8]) -> Vec<ByteRange> {
    let mut out = Vec::new();
    let mut start = 0u32;
    for (i, b) in src.iter().enumerate() {
        if *b == b'\n' {
            let end = (i + 1) as u32;
            out.push(ByteRange { start, end });
            start = end;
        }
    }
    if (start as usize) < src.len() {
        out.push(ByteRange {
            start,
            end: src.len() as u32,
        });
    } else if src.is_empty() {
        out.push(ByteRange { start: 0, end: 0 });
    }
    out
}

/// Byte spans of the lines a line diff reports as equal, paired old → new.
pub fn equal_lines(old: &[u8], new: &[u8]) -> Vec<(ByteRange, ByteRange)> {
    let old_lines = line_spans(old);
    let new_lines = line_spans(new);
    let old_s = String::from_utf8_lossy(old);
    let new_s = String::from_utf8_lossy(new);
    let diff = TextDiff::from_lines(old_s.as_ref(), new_s.as_ref());
    let (mut oi, mut ni) = (0usize, 0usize);
    let mut pairs = Vec::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let (Some(o), Some(n)) = (old_lines.get(oi), new_lines.get(ni)) {
                    pairs.push((*o, *n));
                }
                oi += 1;
                ni += 1;
            }
            ChangeTag::Delete => oi += 1,
            ChangeTag::Insert => ni += 1,
        }
    }
    pairs
}

/// First occurrence of each local slot in a render map: its binder site.
pub fn binder_sites(map: &[(ByteRange, IdentRef)]) -> HashMap<SlotKey, ByteRange> {
    let mut sites = HashMap::new();
    for (r, ident) in map {
        if let IdentRef::Local(s, ns) = ident {
            sites.entry((*s, *ns)).or_insert(*r);
        }
    }
    sites
}

pub fn is_site(range: ByteRange, key: SlotKey, sites: &HashMap<SlotKey, ByteRange>) -> bool {
    sites.get(&key) == Some(&range)
}

/// Map entries inside `line`, in byte order.
pub fn idents_in(map: &[(ByteRange, IdentRef)], line: ByteRange) -> Vec<(ByteRange, IdentRef)> {
    let mut hits: Vec<_> = map
        .iter()
        .filter(|(r, _)| line.start <= r.start && r.end <= line.end)
        .cloned()
        .collect();
    hits.sort_by_key(|(r, _)| r.start);
    hits
}

/// Old slot → new slot, for binders whose declaration sites sit on lines the
/// diff calls equal. Slots only appearing on changed lines are unmapped.
pub fn slot_bijection(
    pairs: &[(ByteRange, ByteRange)],
    old_map: &[(ByteRange, IdentRef)],
    new_map: &[(ByteRange, IdentRef)],
) -> HashMap<SlotKey, SlotKey> {
    let old_sites = binder_sites(old_map);
    let new_sites = binder_sites(new_map);
    let mut out = HashMap::new();
    for (o_line, n_line) in pairs {
        let o_ids = idents_in(old_map, *o_line);
        let n_ids = idents_in(new_map, *n_line);
        for ((or, o), (nr, n)) in o_ids.iter().zip(n_ids.iter()) {
            if let (IdentRef::Local(os, ons), IdentRef::Local(ns, nns)) = (o, n)
                && is_site(*or, (*os, *ons), &old_sites)
                && is_site(*nr, (*ns, *nns), &new_sites)
            {
                out.entry((*os, *ons)).or_insert((*ns, *nns));
            }
        }
    }
    out
}

/// Extend `bijection` with binders whose declaration line changed but whose
/// spelling names exactly one binder on each side: an edited initializer or a
/// `?` turned into `.map_err(..)?` does not move the uses of that binder to another
/// declaration. A name with two binders on either side (a shadow) stays unmapped.
pub fn pair_unmapped_by_spelling(
    bijection: &mut HashMap<SlotKey, SlotKey>,
    old_render: &[u8],
    new_render: &[u8],
    old_map: &[(ByteRange, IdentRef)],
    new_map: &[(ByteRange, IdentRef)],
) {
    let unique = |render: &[u8], map: &[(ByteRange, IdentRef)]| {
        let mut by_name: HashMap<(Vec<u8>, Namespace), Vec<SlotKey>> = HashMap::new();
        for (key, site) in binder_sites(map) {
            let text = render[site.start as usize..site.end as usize].to_vec();
            by_name.entry((text, key.1)).or_default().push(key);
        }
        by_name
            .into_iter()
            .filter_map(|(name, keys)| match keys.as_slice() {
                [one] => Some((name, *one)),
                _ => None,
            })
            .collect::<HashMap<_, _>>()
    };
    let old_unique = unique(old_render, old_map);
    let new_unique = unique(new_render, new_map);
    let mapped_new: std::collections::HashSet<SlotKey> = bijection.values().copied().collect();
    for (name, old_key) in old_unique {
        if bijection.contains_key(&old_key) {
            continue;
        }
        if let Some(new_key) = new_unique.get(&name)
            && !mapped_new.contains(new_key)
        {
            bijection.insert(old_key, *new_key);
        }
    }
}

/// Extend `bijection` with binders whose declaration *line* is unique on both
/// sides even if it moved. Reordering `let foo_value` / `let bar_value` keeps
/// each slot; reordering two `let x` shadows maps each initializer to itself
/// so a later use that now sees the other `x` is a Binding, not a silent slot
/// number coincidence.
pub fn pair_unmapped_by_unique_line(
    bijection: &mut HashMap<SlotKey, SlotKey>,
    old_render: &[u8],
    new_render: &[u8],
    old_map: &[(ByteRange, IdentRef)],
    new_map: &[(ByteRange, IdentRef)],
) {
    let old_lines = line_spans(old_render);
    let new_lines = line_spans(new_render);
    let old_counts = line_text_counts(old_render, &old_lines);
    let new_counts = line_text_counts(new_render, &new_lines);
    let new_sites = binder_sites(new_map);
    let mapped_new: std::collections::HashSet<SlotKey> = bijection.values().copied().collect();
    for (old_key, old_site) in binder_sites(old_map) {
        if bijection.contains_key(&old_key) {
            continue;
        }
        let Some(text) = line_covering(old_render, &old_lines, old_site) else {
            continue;
        };
        if old_counts.get(text) != Some(&1) || new_counts.get(text) != Some(&1) {
            continue;
        }
        let Some((new_key, _)) = new_sites.iter().find(|(_, site)| {
            line_covering(new_render, &new_lines, **site) == Some(text)
        }) else {
            continue;
        };
        if mapped_new.contains(new_key) {
            continue;
        }
        bijection.insert(old_key, *new_key);
    }
}

fn line_covering<'a>(src: &'a [u8], lines: &[ByteRange], r: ByteRange) -> Option<&'a [u8]> {
    let line = lines.iter().find(|l| l.start <= r.start && r.end <= l.end)?;
    src.get(line.start as usize..line.end as usize)
}

fn line_text_counts<'a>(src: &'a [u8], lines: &[ByteRange]) -> HashMap<&'a [u8], usize> {
    let mut c = HashMap::new();
    for l in lines {
        if let Some(t) = src.get(l.start as usize..l.end as usize) {
            *c.entry(t).or_insert(0) += 1;
        }
    }
    c
}

/// Carry a byte range from `from` into `to` through the equal lines, keeping
/// its offset within the line. `None` if its line was changed.
pub fn map_range(from: &[u8], to: &[u8], r: ByteRange) -> Option<ByteRange> {
    let (o, n) = equal_lines(from, to)
        .into_iter()
        .find(|(o, _)| o.start <= r.start && r.end <= o.end)?;
    let start = n.start + (r.start - o.start);
    Some(ByteRange {
        start,
        end: start + r.len(),
    })
}
