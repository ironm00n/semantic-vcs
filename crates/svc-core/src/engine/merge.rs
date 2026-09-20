use std::collections::{BTreeMap, BTreeSet, HashMap};

use similar::{MergeResolution, TextMerge};

use crate::content::IdentRef;
use crate::entity::{EntityRecord, FileRecord, Kind, SigKey};
use crate::error::{Error, Result};
use crate::ids::{AtomIx, ByteRange, EntityId, LineCol, RelPath, SnapshotId, TokenIx};
use crate::lang::Langs;
use crate::snapshot::{AttrValue, Conflict, Hunk, Merge, Side, Snapshot};
use crate::store::Store;

use super::align::{equal_lines, map_range, slot_bijection};
use super::{
    env_from_snapshot, fill_nested_items_from_snapshot, fill_self_methods_from_snapshot,
    ingest_file_with_env, parse, render_entity,
};

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
        let xb = b_s.entities.get(&id).or_else(|| {
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
                        key: ra.sig_key(),
                        a: id,
                        b: id,
                    });
                    entities.insert(id, ra.clone());
                }
            }
            (Some(ro), Some(ra), Some(rb)) => {
                let rb = rewrite_record(rb, &rewrite);
                let parent_kind = ra.parent.and_then(|p| a_s.entities.get(&p).map(|r| r.kind));
                let mut env = env_from_snapshot(&a_s);
                env.current_file = Some(ra.file.clone());
                fill_self_methods_from_snapshot(&mut env, &a_s, ra.parent);
                fill_nested_items_from_snapshot(&mut env, &a_s, id);
                let rec = merge_record(
                    store,
                    langs,
                    &env,
                    ro,
                    ra,
                    &rb,
                    id,
                    parent_kind,
                    &render_entity(&base_s, store, id, false)?.0,
                    &render_entity(&a_s, store, id, false)?.0,
                    &render_entity(&b_s, store, id, false)?.0,
                    &mut conflicts,
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
    signature_pass(&mut snap, &base_s);
    binding_post(store, langs, &base_s, &a_s, &b_s, &mut snap)?;
    Ok(snap)
}

fn unify_add_add(a: &Snapshot, b: &Snapshot, rewrite: &mut HashMap<EntityId, EntityId>) {
    let a_by: HashMap<_, _> = a
        .entities
        .iter()
        .map(|(id, rec)| (rec.sig_key(), *id))
        .collect();
    for (bid, rec) in &b.entities {
        if a.entities.contains_key(bid) {
            continue;
        }
        if let Some(aid) = a_by.get(&rec.sig_key())
            && !b.entities.contains_key(aid)
        {
            rewrite.insert(*bid, *aid);
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
    parent_kind: Option<Kind>,
    o_src: &[u8],
    a_src: &[u8],
    b_src: &[u8],
    conflicts: &mut Vec<Conflict>,
) -> Result<EntityRecord> {
    let sides = (o, a, b);
    let name = merge_attr(sides, id, conflicts, |r| r.name.clone(), AttrValue::Name);
    let parent = merge_attr(sides, id, conflicts, |r| r.parent, AttrValue::Parent);
    let file = merge_attr(sides, id, conflicts, |r| r.file.clone(), AttrValue::File);
    let ordinal = if super::diff_impl::commutative_layout(a.file.extension(), parent_kind, a.kind) {
        three(o.ordinal, a.ordinal, b.ordinal).unwrap_or(a.ordinal)
    } else {
        merge_attr(sides, id, conflicts, |r| r.ordinal, AttrValue::Ordinal)
    };
    let body = |r: &EntityRecord| (r.content, r.bytes);
    let (content, bytes) = if body(a) == body(o) || body(a) == body(b) {
        body(b)
    } else if body(b) == body(o) {
        body(a)
    } else {
        merge_content(store, langs, env, a, id, o_src, a_src, b_src, conflicts)?
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

/// Three-way merge of one record attribute; on a real conflict, record it and keep A's.
fn merge_attr<T: PartialEq + Clone>(
    (o, a, b): (&EntityRecord, &EntityRecord, &EntityRecord),
    id: EntityId,
    conflicts: &mut Vec<Conflict>,
    get: impl Fn(&EntityRecord) -> T,
    wrap: impl Fn(T) -> AttrValue,
) -> T {
    let (vo, va, vb) = (get(o), get(a), get(b));
    three(vo.clone(), va.clone(), vb.clone()).unwrap_or_else(|| {
        conflicts.push(Conflict::Attr {
            id,
            sides: Merge::three_way(wrap(vo), wrap(va.clone()), wrap(vb)),
        });
        va
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
    a: &EntityRecord,
    id: EntityId,
    o_src: &[u8],
    a_src: &[u8],
    b_src: &[u8],
    conflicts: &mut Vec<Conflict>,
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
    for region in merged.regions() {
        let (srcs, range) = match region.resolution() {
            MergeResolution::Unchanged | MergeResolution::Both => (&oa, region.base_range()),
            MergeResolution::Theirs => (&ba, region.theirs_range()),
            _ => (&aa, region.ours_range()),
        };
        for idx in range {
            if let Some(atom) = srcs.get(idx) {
                out.extend_from_slice(atom);
            }
        }
    }
    // A throwaway snapshot: only the re-ingested content/bytes hashes are kept.
    let part = ingest_file_with_env(
        &out,
        a.file.clone(),
        lang,
        store,
        crate::ids::ChangeId::new(),
        env,
    )?;
    let rec = part
        .entities
        .values()
        .find(|r| r.parent.is_none())
        .ok_or_else(|| Error::Other("merged item produced no entity".into()))?;
    Ok((rec.content, rec.bytes))
}

/// The item node of a rendered entity: the first top-level node that is an entity kind
/// for `lang`. Leading doc comments and attributes belong to the entity's bytes, so
/// `root.child(0)` is not it.
fn item_node<'t>(
    tree: &'t tree_sitter::Tree,
    lang: &dyn crate::lang::Lang,
) -> Option<tree_sitter::Node<'t>> {
    let root = tree.root_node();
    let mut c = root.walk();
    root.named_children(&mut c)
        .find(|n| lang.entity_kinds().iter().any(|r| r.node_kind == n.kind()))
}

fn src_atoms(src: &[u8], lang: &dyn crate::lang::Lang) -> Result<Vec<Vec<u8>>> {
    let tree = parse(src, lang)?;
    let Some(item) = item_node(&tree, lang) else {
        return Ok(vec![src.to_vec()]);
    };
    let Some(body) = item.child_by_field_name("body") else {
        return Ok(vec![src.to_vec()]);
    };
    // Atom 0 is the signature up to and including the body's opening brace.
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

fn merge_files(o: &Snapshot, a: &Snapshot, b: &Snapshot) -> BTreeMap<RelPath, FileRecord> {
    let paths: BTreeSet<_> = o
        .files
        .keys()
        .chain(a.files.keys())
        .chain(b.files.keys())
        .cloned()
        .collect();
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

fn signature_pass(snap: &mut Snapshot, base: &Snapshot) {
    let mut seen: HashMap<SigKey, EntityId> = HashMap::new();
    let mut extra = Vec::new();
    for (id, rec) in &snap.entities {
        // Position-named kinds (impl, static blocks, use lines) may legitimately repeat.
        if rec.kind.is_synthetic_named() || rec.kind == Kind::Opaque {
            continue;
        }
        if let Some(prev) = seen.insert(rec.sig_key(), *id) {
            // Two object-literal methods can share `(parent, kind, name)` in a
            // snapshot that already shipped; that is not both sides adding.
            if base.entities.contains_key(&prev) && base.entities.contains_key(id) {
                continue;
            }
            extra.push(Conflict::AddAdd {
                key: rec.sig_key(),
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
) -> Result<()> {
    let base_env = env_from_snapshot(snap);
    let ids: Vec<_> = snap.entities.keys().copied().collect();
    for id in ids {
        let rec = snap.entities[&id].clone();
        // A nested definition may not be valid as a standalone parse (for
        // example, a JavaScript method outside its class). Re-resolving an
        // untouched nested record without its parent context manufactures
        // binding changes. Changed nested records still go through the
        // post-condition; unchanged ones cannot introduce a new local map.
        let origins: Vec<_> = [base, a, b]
            .iter()
            .filter_map(|origin| origin.entities.get(&id))
            .collect();
        if rec.parent.is_some()
            && !origins.is_empty()
            && origins.iter().all(|origin| *origin == &rec)
        {
            continue;
        }
        let Some(lang) = langs.for_path(&rec.file) else {
            continue;
        };
        let (item, map) = render_entity(snap, store, id, true)?;
        let map = map.unwrap_or_default();
        let tree = match super::parse(&item, lang) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let Some(node) = item_node(&tree, lang) else {
            continue;
        };
        let own_name = node
            .child_by_field_name("name")
            .map(super::extract::byte_range);
        let mut env = base_env.clone();
        env.current_file = Some(rec.file.clone());
        fill_self_methods_from_snapshot(&mut env, snap, rec.parent);
        fill_nested_items_from_snapshot(&mut env, snap, id);
        let res = super::resolve(node, &item, lang, &env)?;
        for (i, (r, ident)) in res.refs.iter().enumerate() {
            if own_name == Some(*r) {
                continue;
            }
            let stored: Vec<_> = [base, a, b]
                .into_iter()
                .filter_map(|origin| {
                    stored_ref(origin, store, id, &item, &map, *r)
                        .map(|(was, was_at)| (origin, was, was_at))
                })
                .collect();
            // The declaration site is stored as `Entity(SELF)` (§2.3) and re-resolves to the
            // entity's own id; that is the same target, not a rebinding.
            let now = match ident {
                IdentRef::Entity(x) if *x == id => IdentRef::Entity(EntityId::SELF),
                other => other.clone(),
            };
            if let Some((_, was, was_at)) = stored.iter().find(|(_, was, _)| !ref_eq(was, &now, id))
                && !stored
                    .iter()
                    .any(|(origin, was, _)| ref_eq(was, &now, id) && ident_still_bound(was, origin))
            {
                snap.conflicts.push(Conflict::Binding {
                    id,
                    ident: TokenIx(i as u32),
                    name: source_name(&item, *r).unwrap_or_else(|| ref_name(ident)),
                    at: line_col(&item, r.start),
                    was: was.clone(),
                    was_at: Some(*was_at),
                    now,
                    now_at: Some(line_col(&item, r.start)),
                });
            }
        }
    }
    Ok(())
}

fn stored_ref(
    origin: &Snapshot,
    store: &dyn Store,
    id: EntityId,
    merged_src: &[u8],
    merged_map: &[(ByteRange, IdentRef)],
    r: ByteRange,
) -> Option<(IdentRef, LineCol)> {
    if !origin.entities.contains_key(&id) {
        return None;
    }
    let (orig_src, orig_map) = super::render_entity(origin, store, id, true).ok()?;
    let orig_map = orig_map.unwrap_or_default();
    let mapped = map_range(merged_src, &orig_src, r)?;
    let at = line_col(&orig_src, mapped.start);
    let ident = match ident_at(&orig_map, mapped)? {
        IdentRef::Local(slot, ns) => {
            let slots = slot_bijection(&equal_lines(&orig_src, merged_src), &orig_map, merged_map);
            let (slot, ns) = slots.get(&(slot, ns)).copied().unwrap_or((slot, ns));
            IdentRef::Local(slot, ns)
        }
        other => other,
    };
    Some((ident, at))
}

fn ident_at(map: &[(ByteRange, IdentRef)], r: ByteRange) -> Option<IdentRef> {
    map.iter()
        .find(|(mr, _)| mr.start == r.start && mr.end == r.end)
        .or_else(|| {
            map.iter()
                .find(|(mr, _)| mr.start < r.end && r.start < mr.end)
        })
        .map(|(_, i)| i.clone())
}

fn ident_still_bound(was: &IdentRef, origin: &Snapshot) -> bool {
    match was {
        IdentRef::Entity(eid) if *eid == EntityId::SELF => true,
        IdentRef::Entity(eid) => origin.entities.contains_key(eid),
        IdentRef::Local(_, _) => true,
        IdentRef::Free(_) => false,
    }
}

fn ref_eq(a: &IdentRef, b: &IdentRef, owner: EntityId) -> bool {
    match (a, b) {
        (IdentRef::Entity(x), IdentRef::Entity(y)) => {
            let x = if *x == EntityId::SELF { owner } else { *x };
            let y = if *y == EntityId::SELF { owner } else { *y };
            x == y
        }
        (IdentRef::Free(x), IdentRef::Free(y)) => x == y,
        (IdentRef::Local(x, xn), IdentRef::Local(y, yn)) => x == y && xn == yn,
        _ => false,
    }
}

fn source_name(item: &[u8], r: ByteRange) -> Option<String> {
    let start = r.start as usize;
    let end = r.end as usize;
    let slice = item.get(start..end)?;
    let s = std::str::from_utf8(slice).ok()?;
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
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
