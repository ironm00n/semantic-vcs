use crate::delta::Delta;
use crate::entity::Kind;
use crate::lang::Lang;
use crate::snapshot::Snapshot;
use crate::{JsLang, RustLang};

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

pub(crate) fn commutative_layout(ext: Option<&str>, parent: Option<Kind>, child: Kind) -> bool {
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
