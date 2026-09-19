use crate::delta::Delta;
use crate::entity::Kind;
use crate::ids::EntityId;
use crate::ids::{BytesId, ContentId};
use crate::lang::{Lang, RawEntity};
use crate::snapshot::Snapshot;
use crate::{JsLang, RustLang};

pub fn match_entities(
    prev: &Snapshot,
    parsed: &[RawEntity],
    hashes: &[(ContentId, BytesId)],
) -> Vec<(usize, Option<EntityId>)> {
    let mut used = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(parsed.len());
    for (i, ent) in parsed.iter().enumerate() {
        let by_name = prev.entities.iter().find(|(id, rec)| {
            !used.contains(*id)
                && rec.name == ent.name
                && rec.kind == ent.kind
                && rec.parent.is_some() == ent.parent_idx.is_some()
        });
        if let Some((id, _)) = by_name {
            used.insert(*id);
            out.push((i, Some(*id)));
            continue;
        }
        let hash = hashes.get(i).map(|(c, _)| *c);
        let mut hits: Vec<EntityId> = prev
            .entities
            .iter()
            .filter(|(id, rec)| !used.contains(*id) && Some(rec.content) == hash)
            .map(|(id, _)| *id)
            .collect();
        if hits.len() == 1 {
            let id = hits.pop().unwrap();
            used.insert(id);
            out.push((i, Some(id)));
        } else {
            out.push((i, None));
        }
    }
    out
}

pub fn diff(prev: &Snapshot, next: &Snapshot) -> Vec<Delta> {
    let mut out = Vec::new();
    for (id, rec) in &next.entities {
        match prev.entities.get(id) {
            None => out.push(Delta::Added(*id)),
            Some(old) => {
                if old.name != rec.name {
                    out.push(Delta::Renamed {
                        id: *id,
                        from: old.name.clone(),
                        to: rec.name.clone(),
                    });
                }
                if old.parent != rec.parent {
                    out.push(Delta::Moved {
                        id: *id,
                        from_parent: old.parent,
                        to_parent: rec.parent,
                    });
                }
                if old.file != rec.file || old.ordinal != rec.ordinal {
                    let parent_kind = rec
                        .parent
                        .and_then(|pid| next.entities.get(&pid).map(|p| p.kind));
                    let layout_only = old.parent == rec.parent
                        && old.file == rec.file
                        && commutative_layout(rec.file.extension(), parent_kind, rec.kind);
                    if !layout_only {
                        out.push(Delta::Relocated {
                            id: *id,
                            from: (old.file.clone(), old.ordinal),
                            to: (rec.file.clone(), rec.ordinal),
                        });
                    }
                }
                if old.content != rec.content {
                    out.push(Delta::Edited(
                        *id,
                        crate::delta::ObservedClass::BindingPreserving,
                    ));
                } else if old.bytes != rec.bytes {
                    out.push(Delta::Edited(*id, crate::delta::ObservedClass::Alpha));
                }
            }
        }
    }
    for id in prev.entities.keys() {
        if !next.entities.contains_key(id) {
            out.push(Delta::Removed(*id));
        }
    }
    out
}

fn commutative_layout(ext: Option<&str>, parent: Option<Kind>, child: Kind) -> bool {
    let rules = match ext {
        Some("rs") => RustLang.commutative_parents(),
        Some("js" | "mjs" | "cjs") => JsLang.commutative_parents(),
        _ => return false,
    };
    rules.iter().any(|rule| {
        rule.parent == parent
            && rule
                .only_child_kinds
                .is_none_or(|kinds| kinds.contains(&child))
            && !rule
                .except_child_kinds
                .is_some_and(|kinds| kinds.contains(&child))
    })
}
