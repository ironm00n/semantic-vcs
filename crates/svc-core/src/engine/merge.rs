use std::collections::{BTreeMap, BTreeSet, HashMap};

use similar::{ChangeTag, MergeResolution, TextDiff, TextMerge};

use crate::content::IdentRef;
use crate::entity::{EntityRecord, FileRecord, SigKey};
use crate::error::{Error, Result};
use crate::ids::{AtomIx, ByteRange, EntityId, LineCol, RelPath, SnapshotId, TokenIx};
use crate::lang::Langs;
use crate::snapshot::{Conflict, Hunk, Merge, Side, Snapshot};
use crate::store::Store;

use super::{env_from_snapshot, ingest_file_with_env, parse, render, render_entity};

/// Per-entity 3-way merge plus the §5.4 binding post-condition.
pub fn merge(
    store: &dyn Store,
    langs: &Langs,
    base: SnapshotId,
    a: SnapshotId,
    b: SnapshotId,
) -> Result<Snapshot> {
    let base_s = store.get_snapshot(base)?;
    let a_s = store.get_snapshot(a)?;
    let b_s = store.get_snapshot(b)?;
    let mut rewrite = HashMap::new();
    unify_add_add(&a_s, &b_s, &mut rewrite);

    let mut entities: BTreeMap<EntityId, EntityRecord> = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut atom_side: HashMap<(EntityId, usize), SideOrBase> = HashMap::new();

    let ids: BTreeSet<EntityId> = base_s
        .entities
        .keys()
        .chain(a_s.entities.keys())
        .chain(b_s.entities.keys())
        .copied()
        .map(|id| *rewrite.get(&id).unwrap_or(&id))
        .collect();

    for id in ids {
        let o = base_s.entities.get(&id);
        let xa = a_s.entities.get(&id);
        let xb = b_s
            .entities
            .get(&id)
            .or_else(|| {
                rewrite
                    .iter()
                    .find(|(_, v)| **v == id)
                    .and_then(|(k, _)| b_s.entities.get(k))
            });
        match (o, xa, xb) {
            (None, None, None) => {}
            (None, Some(rec), None) | (Some(_), Some(rec), None) if xb.is_none() && o.is_none() => {
                entities.insert(id, rec.clone());
            }
            (None, None, Some(rec)) => {
                entities.insert(id, rewrite_record(rec, &rewrite));
            }
            (Some(_), None, None) => {
                // deleted both
            }
            // Delete/edit keeps the edited record so the conflict can be resolved by choosing.
            (Some(ro), None, Some(rb)) => {
                let rb = rewrite_record(rb, &rewrite);
                if rb == *ro {
                    continue;
                }
                conflicts.push(Conflict::DeleteEdit {
                    id,
                    deleted_by: Side::A,
                    edited_by: Side::B,
                });
                entities.insert(id, rb);
            }
            (Some(ro), Some(ra), None) => {
                if ra == ro {
                    continue;
                }
                conflicts.push(Conflict::DeleteEdit {
                    id,
                    deleted_by: Side::B,
                    edited_by: Side::A,
                });
                entities.insert(id, ra.clone());
            }
            (None, Some(ra), Some(rb)) => {
                let rb = rewrite_record(rb, &rewrite);
                if ra.content == rb.content && ra.bytes == rb.bytes {
                    entities.insert(id, ra.clone());
                } else {
                    conflicts.push(Conflict::AddAdd {
                        key: SigKey {
                            parent: ra.parent,
                            kind: ra.kind,
                            name: ra.name.clone(),
                        },
                        a: id,
                        b: id,
                    });
                    entities.insert(id, ra.clone());
                }
            }
            (Some(ro), Some(ra), Some(rb)) => {
                let rb = rewrite_record(rb, &rewrite);
                let rec = merge_record(
                    store,
                    langs,
                    &env_from_snapshot(&a_s),
                    ro,
                    ra,
                    &rb,
                    id,
                    &render_entity(&base_s, store, id, false)?.0,
                    &render_entity(&a_s, store, id, false)?.0,
                    &render_entity(&b_s, store, id, false)?.0,
                    &mut conflicts,
                    &mut atom_side,
                )?;
                entities.insert(id, rec);
            }
            _ => {}
        }
    }

    let files = merge_files(&base_s, &a_s, &b_s);
    let change = a_s.change;
    let mut snap = Snapshot {
        parents: vec![a, b],
        predecessors: Vec::new(),
        change,
        entities,
        files,
        conflicts,
        message: String::new(),
    };
    signature_pass(&mut snap);
    binding_post(store, langs, &base_s, &a_s, &b_s, &mut snap, &atom_side)?;
    let _ = change;
    Ok(snap)
}

#[derive(Clone, Copy)]
enum SideOrBase {
    Base,
    A,
    B,
}

fn unify_add_add(a: &Snapshot, b: &Snapshot, rewrite: &mut HashMap<EntityId, EntityId>) {
    let key = |rec: &EntityRecord| (rec.parent, rec.kind, rec.name.clone());
    let a_by: HashMap<_, _> = a.entities.iter().map(|(id, rec)| (key(rec), *id)).collect();
    for (bid, rec) in &b.entities {
        if a.entities.contains_key(bid) {
            continue;
        }
        if let Some(aid) = a_by.get(&key(rec)) {
            if !b.entities.contains_key(aid) {
                rewrite.insert(*bid, *aid);
            }
        }
    }
}

fn rewrite_record(rec: &EntityRecord, rewrite: &HashMap<EntityId, EntityId>) -> EntityRecord {
    let mut rec = rec.clone();
    if let Some(p) = rec.parent {
        rec.parent = Some(*rewrite.get(&p).unwrap_or(&p));
    }
    rec
}

fn merge_record(
    store: &dyn Store,
    langs: &Langs,
    env: &crate::lang::Env,
    o: &EntityRecord,
    a: &EntityRecord,
    b: &EntityRecord,
    id: EntityId,
    o_src: &[u8],
    a_src: &[u8],
    b_src: &[u8],
    conflicts: &mut Vec<Conflict>,
    atom_side: &mut HashMap<(EntityId, usize), SideOrBase>,
) -> Result<EntityRecord> {
    let name = three(o.name.clone(), a.name.clone(), b.name.clone()).unwrap_or_else(|| {
        conflicts.push(Conflict::Attr {
            id,
            sides: Merge::three_way(
                crate::snapshot::AttrValue::Name(o.name.clone()),
                crate::snapshot::AttrValue::Name(a.name.clone()),
                crate::snapshot::AttrValue::Name(b.name.clone()),
            ),
        });
        a.name.clone()
    });
    let parent = three(o.parent, a.parent, b.parent).unwrap_or_else(|| {
        conflicts.push(Conflict::Attr {
            id,
            sides: Merge::three_way(
                crate::snapshot::AttrValue::Parent(o.parent),
                crate::snapshot::AttrValue::Parent(a.parent),
                crate::snapshot::AttrValue::Parent(b.parent),
            ),
        });
        a.parent
    });
    let file = three(o.file.clone(), a.file.clone(), b.file.clone()).unwrap_or_else(|| {
        conflicts.push(Conflict::Attr {
            id,
            sides: Merge::three_way(
                crate::snapshot::AttrValue::File(o.file.clone()),
                crate::snapshot::AttrValue::File(a.file.clone()),
                crate::snapshot::AttrValue::File(b.file.clone()),
            ),
        });
        a.file.clone()
    });
    let ordinal = three(o.ordinal, a.ordinal, b.ordinal).unwrap_or_else(|| {
        conflicts.push(Conflict::Attr {
            id,
            sides: Merge::three_way(
                crate::snapshot::AttrValue::Ordinal(o.ordinal),
                crate::snapshot::AttrValue::Ordinal(a.ordinal),
                crate::snapshot::AttrValue::Ordinal(b.ordinal),
            ),
        });
        a.ordinal
    });
    let (content, bytes) = if a.content == o.content && a.bytes == o.bytes {
        (b.content, b.bytes)
    } else if b.content == o.content && b.bytes == o.bytes {
        (a.content, a.bytes)
    } else if a.content == b.content && a.bytes == b.bytes {
        (a.content, a.bytes)
    } else {
        merge_content(
            store, langs, env, o, a, b, id, o_src, a_src, b_src, conflicts, atom_side,
        )?
    };
    Ok(EntityRecord {
        name,
        kind: a.kind,
        parent,
        file,
        ordinal,
        content,
        bytes,
    })
}

fn three<T: PartialEq>(o: T, a: T, b: T) -> Option<T> {
    if a == b {
        Some(a)
    } else if a == o {
        Some(b)
    } else if b == o {
        Some(a)
    } else {
        None
    }
}

fn merge_content(
    store: &dyn Store,
    langs: &Langs,
    env: &crate::lang::Env,
    o: &EntityRecord,
    a: &EntityRecord,
    b: &EntityRecord,
    id: EntityId,
    o_src: &[u8],
    a_src: &[u8],
    b_src: &[u8],
    conflicts: &mut Vec<Conflict>,
    atom_side: &mut HashMap<(EntityId, usize), SideOrBase>,
) -> Result<(crate::ids::ContentId, crate::ids::BytesId)> {
    let lang = langs
        .for_path(&a.file)
        .ok_or_else(|| Error::NoLanguage(a.file.clone()))?;
    let oa = src_atoms(o_src, lang)?;
    let aa = src_atoms(a_src, lang)?;
    let ba = src_atoms(b_src, lang)?;
    let hash = |atoms: &[Vec<u8>]| {
        atoms
            .iter()
            .map(|s| format!("{}", blake3::hash(s)))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let o_hash = hash(&oa);
    let a_hash = hash(&aa);
    let b_hash = hash(&ba);
    let merged = TextMerge::from_lines(&o_hash, &a_hash, &b_hash);
    if merged.is_conflicted() {
        let mut hunks = Vec::new();
        for region in merged.conflicts() {
            hunks.push(Merge::three_way(
                Hunk {
                    entity: id,
                    start: AtomIx(region.base_range().start as u32),
                    end: AtomIx(region.base_range().end as u32),
                },
                Hunk {
                    entity: id,
                    start: AtomIx(region.ours_range().start as u32),
                    end: AtomIx(region.ours_range().end as u32),
                },
                Hunk {
                    entity: id,
                    start: AtomIx(region.theirs_range().start as u32),
                    end: AtomIx(region.theirs_range().end as u32),
                },
            ));
        }
        conflicts.push(Conflict::Content { id, hunks });
        return Ok((a.content, a.bytes));
    }
    let mut out = Vec::new();
    for (i, region) in merged.regions().iter().enumerate() {
        let (side, srcs, range) = match region.resolution() {
            MergeResolution::Unchanged | MergeResolution::Both => {
                (SideOrBase::Base, &oa, region.base_range())
            }
            MergeResolution::Ours => (SideOrBase::A, &aa, region.ours_range()),
            MergeResolution::Theirs => (SideOrBase::B, &ba, region.theirs_range()),
            MergeResolution::Conflict => (SideOrBase::A, &aa, region.ours_range()),
            _ => (SideOrBase::A, &aa, region.ours_range()),
        };
        atom_side.insert((id, i), side);
        for idx in range {
            if let Some(atom) = srcs.get(idx) {
                out.extend_from_slice(atom);
            }
        }
    }
    let part = ingest_file_with_env(&out, a.file.clone(), lang, store, a_change_dummy(a), env)?;
    let rec = part
        .entities
        .values()
        .find(|r| r.parent.is_none())
        .ok_or_else(|| Error::Other("merged item produced no entity".into()))?;
    let _ = o;
    let _ = b;
    Ok((rec.content, rec.bytes))
}

fn a_change_dummy(a: &EntityRecord) -> crate::ids::ChangeId {
    let _ = a;
    crate::ids::ChangeId::new()
}

fn src_atoms(src: &[u8], lang: &dyn crate::lang::Lang) -> Result<Vec<Vec<u8>>> {
    let tree = parse(src, lang)?;
    let Some(item) = tree.root_node().named_child(0) else {
        return Ok(vec![src.to_vec()]);
    };
    let Some(body) = item.child_by_field_name("body") else {
        return Ok(vec![src.to_vec()]);
    };
    // Atom 0 is the signature up to and including the body's opening brace (SPEC §5.3).
    let open = body.start_byte() + usize::from(src.get(body.start_byte()) == Some(&b'{'));
    let mut atoms = Vec::new();
    atoms.push(src.get(..open).unwrap_or(src).to_vec());
    let mut c = body.walk();
    let mut last = open;
    for ch in body.named_children(&mut c) {
        let start = last;
        let end = ch.end_byte();
        atoms.push(src.get(start..end).unwrap_or(&[]).to_vec());
        last = end;
    }
    let close = body.end_byte();
    if last < close {
        atoms.push(src.get(last..close).unwrap_or(&[]).to_vec());
    }
    Ok(atoms)
}

fn merge_files(
    o: &Snapshot,
    a: &Snapshot,
    b: &Snapshot,
) -> BTreeMap<RelPath, FileRecord> {
    let paths: BTreeSet<_> = o.files.keys().chain(a.files.keys()).chain(b.files.keys()).cloned().collect();
    let mut out = BTreeMap::new();
    for p in paths {
        let of = o.files.get(&p);
        let af = a.files.get(&p);
        let bf = b.files.get(&p);
        match (of, af, bf) {
            (_, Some(x), None) => {
                out.insert(p, x.clone());
            }
            (_, None, Some(x)) => {
                out.insert(p, x.clone());
            }
            (Some(x), Some(y), Some(z)) => {
                let trailing = three(x.trailing.clone(), y.trailing.clone(), z.trailing.clone())
                    .unwrap_or_else(|| y.trailing.clone());
                out.insert(p, FileRecord { trailing });
            }
            (None, Some(y), Some(z)) if y == z => {
                out.insert(p, y.clone());
            }
            (None, Some(y), Some(_)) => {
                out.insert(p, y.clone());
            }
            (Some(x), None, None) => {
                out.insert(p, x.clone());
            }
            _ => {}
        }
    }
    out
}

fn signature_pass(snap: &mut Snapshot) {
    let mut seen: HashMap<(Option<EntityId>, crate::entity::Kind, String), EntityId> = HashMap::new();
    let mut extra = Vec::new();
    for (id, rec) in &snap.entities {
        let key = (rec.parent, rec.kind, rec.name.clone());
        if let Some(prev) = seen.insert(key.clone(), *id) {
            extra.push(Conflict::AddAdd {
                key: SigKey {
                    parent: rec.parent,
                    kind: rec.kind,
                    name: rec.name.clone(),
                },
                a: prev,
                b: *id,
            });
        }
    }
    snap.conflicts.extend(extra);
}

fn binding_post(
    store: &dyn Store,
    langs: &Langs,
    base: &Snapshot,
    a: &Snapshot,
    b: &Snapshot,
    snap: &mut Snapshot,
    atom_side: &HashMap<(EntityId, usize), SideOrBase>,
) -> Result<()> {
    let rendered = render(snap, store, langs, true)?;
    let env = env_from_snapshot(snap);
    let ids: Vec<_> = snap.entities.keys().copied().collect();
    for id in ids {
        let rec = snap.entities[&id].clone();
        let Some(lang) = langs.for_path(&rec.file) else {
            continue;
        };
        let Some(file) = rendered.files.get(&rec.file) else {
            continue;
        };
        let (item, _map) = render_entity(snap, store, id, true)?;
        let tree = match super::parse(&item, lang) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let Some(node) = tree.root_node().child(0) else {
            continue;
        };
        let res = super::resolve(node, &item, lang, &env)?;
        let side_of = |r: ByteRange| {
            let atoms = atom_index_for(&item, r);
            match atom_side.get(&(id, atoms)).copied().unwrap_or(SideOrBase::Base) {
                SideOrBase::A => a,
                SideOrBase::B => b,
                SideOrBase::Base => base,
            }
        };
        for (i, (r, ident)) in res.refs.iter().enumerate() {
            if matches!(ident, IdentRef::Free(_)) {
                continue;
            }
            let origin = side_of(*r);
            let stored = stored_ref(origin, store, id, &item, *r);
            // The declaration site is stored as `Entity(SELF)` (§2.3) and re-resolves to the
            // entity's own id; that is the same target, not a rebinding.
            let now = match ident {
                IdentRef::Entity(x) if *x == id => IdentRef::Entity(EntityId::SELF),
                other => other.clone(),
            };
            if let Some(was) = stored {
                if !ref_eq(&was, &now) {
                    snap.conflicts.push(Conflict::Binding {
                        id,
                        ident: TokenIx(i as u32),
                        name: ref_name(ident),
                        at: line_col(&item, r.start),
                        was: was.clone(),
                        was_at: None,
                        now,
                        now_at: Some(line_col(&item, r.start)),
                    });
                }
            }
        }
        let _ = (file, rec);
    }
    Ok(())
}

fn atom_index_for(src: &[u8], r: ByteRange) -> usize {
    let mut n = 0;
    let mut seen_brace = false;
    for (i, b) in src.iter().enumerate() {
        if !seen_brace && *b == b'{' {
            seen_brace = true;
            n = 1;
            continue;
        }
        if seen_brace && *b == b';' && (i as u32) < r.start {
            n += 1;
        }
    }
    if !seen_brace {
        0
    } else {
        n
    }
}

fn stored_ref(
    origin: &Snapshot,
    store: &dyn Store,
    id: EntityId,
    merged_src: &[u8],
    r: ByteRange,
) -> Option<IdentRef> {
    if !origin.entities.contains_key(&id) {
        return None;
    }
    let (orig_src, orig_map) = super::render_entity(origin, store, id, true).ok()?;
    let orig_map = orig_map.unwrap_or_default();
    let mapped = map_range(merged_src, &orig_src, r)?;
    ident_at(&orig_map, mapped)
}

fn ident_at(map: &[(ByteRange, IdentRef)], r: ByteRange) -> Option<IdentRef> {
    map.iter()
        .find(|(mr, _)| mr.start == r.start && mr.end == r.end)
        .or_else(|| {
            map.iter().find(|(mr, _)| {
                mr.start < r.end && r.start < mr.end
            })
        })
        .map(|(_, i)| i.clone())
}

fn map_range(from: &[u8], to: &[u8], r: ByteRange) -> Option<ByteRange> {
    let from_lines = line_spans(from);
    let to_lines = line_spans(to);
    let fi = from_lines
        .iter()
        .position(|l| l.start <= r.start && r.end <= l.end)?;
    let from_s = String::from_utf8_lossy(from);
    let to_s = String::from_utf8_lossy(to);
    let diff = TextDiff::from_lines(from_s.as_ref(), to_s.as_ref());
    let mut f = 0usize;
    let mut t = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if f == fi {
                    let o = from_lines.get(f)?;
                    let n = to_lines.get(t)?;
                    let off = r.start.saturating_sub(o.start);
                    let len = r.end.saturating_sub(r.start);
                    let start = n.start.saturating_add(off);
                    return Some(ByteRange {
                        start,
                        end: start.saturating_add(len),
                    });
                }
                f += 1;
                t += 1;
            }
            ChangeTag::Delete => f += 1,
            ChangeTag::Insert => t += 1,
        }
    }
    None
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

fn ref_eq(a: &IdentRef, b: &IdentRef) -> bool {
    match (a, b) {
        (IdentRef::Entity(x), IdentRef::Entity(y)) => x == y,
        (IdentRef::Free(x), IdentRef::Free(y)) => x == y,
        (IdentRef::Local(x, xn), IdentRef::Local(y, yn)) => x == y && xn == yn,
        _ => false,
    }
}

fn ref_name(ident: &IdentRef) -> String {
    match ident {
        IdentRef::Free(n) => n.to_string(),
        IdentRef::Local(s, _) => format!("${}", s.0),
        IdentRef::Entity(id) => {
            if *id == EntityId::SELF {
                "self".into()
            } else {
                id.short()
            }
        }
    }
}

fn line_col(src: &[u8], off: u32) -> LineCol {
    let mut line = 1u32;
    let mut col = 0u32;
    for (i, b) in src.iter().enumerate() {
        if i as u32 >= off {
            break;
        }
        if *b == b'\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    LineCol { line, col }
}

/// Lowest common ancestor walking `parents`.
pub fn lca(store: &dyn Store, a: SnapshotId, b: SnapshotId) -> Result<SnapshotId> {
    let mut seen = BTreeSet::new();
    let mut cur = Some(a);
    while let Some(id) = cur {
        seen.insert(id);
        cur = store.get_snapshot(id)?.parents.first().copied();
    }
    let mut cur = Some(b);
    while let Some(id) = cur {
        if seen.contains(&id) {
            return Ok(id);
        }
        cur = store.get_snapshot(id)?.parents.first().copied();
    }
    Err(Error::Other("no common ancestor".into()))
}
