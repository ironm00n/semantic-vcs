use crate::delta::Delta;
use crate::entity::Kind;
use crate::error::Result;
use crate::lang::Lang;
use crate::snapshot::Snapshot;
use crate::store::Store;
use crate::{JsLang, RustLang};

use super::classify_entity;

/// Structural deltas between two snapshots. An edited entity carries the class the
/// classifier assigns to old → new (rendered from `store`), never a placeholder. Files
/// are compared by their bytes outside entities, so an opaque file shows up as a whole.
pub fn diff(store: &dyn Store, prev: &Snapshot, next: &Snapshot) -> Result<Vec<Delta>> {
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
                if old.content != rec.content || old.bytes != rec.bytes {
                    out.push(Delta::Edited(*id, classify_entity(store, prev, next, *id)?));
                }
            }
        }
    }
    for (id, old) in &prev.entities {
        if !next.entities.contains_key(id) {
            out.push(Delta::Removed {
                id: *id,
                name: old.name.clone(),
            });
        }
    }
    for (path, rec) in &next.files {
        match prev.files.get(path) {
            None => out.push(Delta::FileAdded(path.clone())),
            Some(old) if old.trailing != rec.trailing => out.push(Delta::FileTail {
                path: path.clone(),
                whitespace_only: sans_whitespace(&old.trailing) == sans_whitespace(&rec.trailing),
            }),
            Some(_) => {}
        }
    }
    for path in prev.files.keys() {
        if !next.files.contains_key(path) {
            out.push(Delta::FileRemoved(path.clone()));
        }
    }
    Ok(out)
}

pub(crate) fn commutative_layout(ext: Option<&str>, parent: Option<Kind>, child: Kind) -> bool {
    let Some(lang) = lang_for_ext(ext) else {
        return false;
    };
    lang.commutative_parents().iter().any(|rule| {
        rule.parent == parent
            && rule
                .only_child_kinds
                .is_none_or(|kinds| kinds.contains(&child))
            && !rule
                .except_child_kinds
                .is_some_and(|kinds| kinds.contains(&child))
    })
}

fn sans_whitespace(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect()
}

/// The language a file extension belongs to, for diffs that have no `Langs` in hand.
pub(crate) fn lang_for_ext(ext: Option<&str>) -> Option<&'static dyn Lang> {
    match ext {
        Some("rs") => Some(&RustLang),
        Some("js" | "mjs" | "cjs") => Some(&JsLang),
        _ => None,
    }
}
