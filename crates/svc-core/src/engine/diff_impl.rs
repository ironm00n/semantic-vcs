use crate::delta::Delta;
use crate::ids::EntityId;
use crate::lang::RawEntity;
use crate::snapshot::Snapshot;
use crate::ids::{BytesId, ContentId};

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
                    out.push(Delta::Relocated {
                        id: *id,
                        from: (old.file.clone(), old.ordinal),
                        to: (rec.file.clone(), rec.ordinal),
                    });
                }
                if old.content != rec.content {
                    out.push(Delta::Edited(*id, crate::delta::ObservedClass::BindingPreserving));
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
