//! Render walks Child holes, not `EntityRecord.parent`. Move/extract/add-def/delete
//! have to keep those holes in sync or the item vanishes (or double-renders).
use std::collections::BTreeMap;

use svc_core::content::{Chunk, Token};
use svc_core::engine::{
    add_def, delete, extract_hoist, lookup_name, move_def, render, rust_langs, snapshot_files,
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
    let (store, _, snap) = fixture();
    let inner = lookup_name(&snap, "inner").unwrap();
    let outer = lookup_name(&snap, "outer").unwrap();
    let next = extract_hoist(&store, &snap, inner, None, 0).unwrap();
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
    let (store, _, snap) = fixture();
    let log = lookup_name(&snap, "log").unwrap();
    let imp = snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl)
        .map(|(id, _)| *id)
        .unwrap();
    let next = move_def(&store, &snap, log, Some(imp), Some(1)).unwrap();
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
    let (store, _, snap) = fixture();
    let outer = lookup_name(&snap, "outer").unwrap();
    let inner = lookup_name(&snap, "inner").unwrap();
    let err = move_def(&store, &snap, outer, Some(inner), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cycle"), "{err}");
}
