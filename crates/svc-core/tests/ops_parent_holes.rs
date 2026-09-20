//! Render walks Child holes, not `EntityRecord.parent`. Move/extract/add-def/delete
//! have to keep those holes in sync or the item vanishes (or double-renders).
use std::collections::BTreeMap;

use svc_core::content::{Chunk, Token};
use svc_core::engine::{
    add_def, delete, edit_def, extract_hoist, lookup_name, move_def, render, rust_langs, snapshot_files,
};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::store::{MemStore, Store};
use svc_core::{Intent, Kind, Snapshot};

const SRC: &str = r#"fn outer() {
    fn inner() {}
}

fn log() {}

struct S;

impl S {
    fn method() {}
}
"#;

fn fixture() -> (MemStore, svc_core::Langs, Snapshot) {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    (store, langs, snap)
}

fn rendered(store: &MemStore, snap: &Snapshot) -> String {
    let langs = rust_langs();
    let out = render(snap, store, &langs, false).unwrap();
    String::from_utf8(out.files.into_values().next().unwrap()).unwrap()
}

fn child_ids(store: &MemStore, snap: &Snapshot, parent: EntityId) -> Vec<EntityId> {
    let rec = &snap.entities[&parent];
    store
        .get_bytes_blob(rec.bytes)
        .unwrap()
        .chunks()
        .iter()
        .filter_map(|c| match c {
            Chunk::Child(id) => Some(*id),
            _ => None,
        })
        .collect()
}

fn assert_holes(store: &MemStore, snap: &Snapshot) {
    for (id, rec) in &snap.entities {
        if let Some(p) = rec.parent {
            let n = child_ids(store, snap, p)
                .iter()
                .filter(|c| *c == id)
                .count();
            assert_eq!(n, 1, "{} must be a Child hole of its parent once", rec.name);
            let content = store.get_content(snap.entities[&p].content).unwrap();
            let n = content
                .tokens
                .iter()
                .filter(|t| matches!(t, Token::Child(c) if c == id))
                .count();
            assert_eq!(n, 1, "{} must be a Content Child of its parent once", rec.name);
        }
    }
    for (pid, rec) in &snap.entities {
        for cid in child_ids(store, snap, *pid) {
            assert_eq!(
                snap.entities[&cid].parent,
                Some(*pid),
                "Child hole in {} must name an entity whose parent is that record ({})",
                rec.name,
                snap.entities[&cid].name
            );
        }
    }
}

#[test]
fn extract_hoists_inner_out_of_outer() {
    let (store, langs, snap) = fixture();
    let inner = lookup_name(&snap, "inner").unwrap();
    let outer = lookup_name(&snap, "outer").unwrap();
    let next = extract_hoist(&store, &langs, &snap, inner, None, 0).unwrap();
    assert!(next.entities[&inner].parent.is_none());
    assert_holes(&store, &next);
    let src = rendered(&store, &next);
    assert!(src.contains("fn inner() {}"), "{src}");
    assert!(
        !child_ids(&store, &next, outer).contains(&inner),
        "outer still has a Child hole for inner"
    );
    // inner is a file root, so it must appear outside outer's body.
    let outer_txt = {
        let (b, _) = svc_core::engine::render_entity(&next, &store, outer, false).unwrap();
        String::from_utf8(b).unwrap()
    };
    assert!(!outer_txt.contains("fn inner"), "{outer_txt}");
}

#[test]
fn move_into_an_impl_renders_inside_it() {
    let (store, langs, snap) = fixture();
    let log = lookup_name(&snap, "log").unwrap();
    let imp = snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl)
        .map(|(id, _)| *id)
        .unwrap();
    let next = move_def(&store, &langs, &snap, log, Some(imp), Some(1)).unwrap();
    assert_eq!(next.entities[&log].parent, Some(imp));
    assert_holes(&store, &next);
    let src = rendered(&store, &next);
    assert!(src.contains("fn log() {}"), "{src}");
    let impl_txt = {
        let (b, _) = svc_core::engine::render_entity(&next, &store, imp, false).unwrap();
        String::from_utf8(b).unwrap()
    };
    assert!(impl_txt.contains("fn log()"), "{impl_txt}");
    assert!(
        !next.file_roots(&next.entities[&log].file).contains(&log),
        "nested log must not also be a file root"
    );
}

#[test]
fn add_def_under_impl_renders_inside_it() {
    let (store, langs, snap) = fixture();
    let imp = snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl)
        .map(|(id, _)| *id)
        .unwrap();
    let id = EntityId::new();
    let next = add_def(
        &store,
        &langs,
        &snap,
        id,
        Some(imp),
        1,
        b"fn extra() {}\n",
        Intent::Feature,
    )
    .unwrap();
    assert_eq!(next.entities[&id].parent, Some(imp));
    assert_holes(&store, &next);
    let impl_txt = {
        let (b, _) = svc_core::engine::render_entity(&next, &store, imp, false).unwrap();
        String::from_utf8(b).unwrap()
    };
    assert!(impl_txt.contains("fn extra()"), "{impl_txt}");
}

#[test]
fn delete_nested_does_not_leave_a_dangling_child_hole() {
    let (store, _, snap) = fixture();
    let inner = lookup_name(&snap, "inner").unwrap();
    let outer = lookup_name(&snap, "outer").unwrap();
    let next = delete(&snap, &store, inner).unwrap();
    assert!(!next.entities.contains_key(&inner));
    assert_holes(&store, &next);
    assert!(!child_ids(&store, &next, outer).contains(&inner));
    let src = rendered(&store, &next);
    assert!(!src.contains("fn inner"), "{src}");
    assert!(src.contains("fn outer"), "{src}");
}

#[test]
fn move_under_a_descendant_is_refused() {
    let (store, langs, snap) = fixture();
    let outer = lookup_name(&snap, "outer").unwrap();
    let inner = lookup_name(&snap, "inner").unwrap();
    let err = move_def(&store, &langs, &snap, outer, Some(inner), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn add_def_of_an_impl_keeps_its_methods() {
    let (store, langs, snap) = fixture();
    let id = EntityId::new();
    let src = b"impl Extra {\n    fn extra() {}\n}\n";
    let next = add_def(&store, &langs, &snap, id, None, 20, src, Intent::Feature).unwrap();
    let extra = next
        .entities
        .iter()
        .find(|(_, r)| r.name == "extra")
        .unwrap_or_else(|| panic!("method extra missing: {:?}", next.entities.values().map(|r| &r.name).collect::<Vec<_>>()));
    assert_eq!(extra.1.parent, Some(id));
    assert_holes(&store, &next);
    let rendered = rendered(&store, &next);
    assert!(rendered.contains("fn extra"), "{rendered}");
    let again = add_def(&store, &langs, &snap, id, None, 20, src, Intent::Feature).unwrap();
    let extra2 = again
        .entities
        .iter()
        .find(|(_, r)| r.name == "extra")
        .map(|(i, _)| *i)
        .unwrap();
    assert_eq!(*extra.0, extra2, "nested ids must be derived from the AddDef id");
}

fn impl_s(snap: &Snapshot) -> EntityId {
    *snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl && r.name.contains('S'))
        .unwrap_or_else(|| panic!("impl S missing: {:?}", snap.entities.values().map(|r| &r.name).collect::<Vec<_>>()))
        .0
}

#[test]
fn edit_def_of_an_impl_keeps_existing_methods() {
    let (store, langs, snap) = fixture();
    let imp = impl_s(&snap);
    let method = lookup_name(&snap, "method").unwrap();
    let src = b"impl S {\n    fn method() {}\n    fn extra() {}\n}\n";
    let (next, _) = edit_def(&store, &langs, &snap, imp, src).unwrap();
    assert_eq!(
        lookup_name(&next, "method").unwrap(),
        method,
        "edit-def must not re-identify a method that stayed"
    );
    let extra = next
        .entities
        .iter()
        .find(|(_, r)| r.name == "extra")
        .unwrap_or_else(|| panic!("method extra missing after edit-def"));
    assert_eq!(extra.1.parent, Some(imp));
    assert_holes(&store, &next);
    let rendered = rendered(&store, &next);
    assert!(rendered.contains("fn extra"), "{rendered}");
    assert!(rendered.contains("fn method"), "{rendered}");
    let (again, _) = edit_def(&store, &langs, &snap, imp, src).unwrap();
    let extra2 = again
        .entities
        .iter()
        .find(|(_, r)| r.name == "extra")
        .map(|(i, _)| *i)
        .unwrap();
    assert_eq!(*extra.0, extra2, "new nested ids must be derived from the edited entity");
}

#[test]
fn edit_def_of_an_impl_drops_removed_methods() {
    let (store, langs, snap) = fixture();
    let imp = impl_s(&snap);
    let (next, _) = edit_def(&store, &langs, &snap, imp, b"impl S {\n}\n").unwrap();
    assert!(
        next.entities.values().all(|r| r.name != "method"),
        "removed method must leave the snapshot"
    );
    assert_holes(&store, &next);
    let rendered = rendered(&store, &next);
    assert!(!rendered.contains("fn method"), "{rendered}");
}
