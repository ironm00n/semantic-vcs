//! `(parent, kind, name)` identifies an entity across re-parses and in merge, so the
//! verbs that could create a second one refuse. `edit_def` keeps the kind it was given.
use std::collections::BTreeMap;

use svc_core::engine::{
    add_def, edit_def, lookup_name, move_def, rename, rust_langs, snapshot_files,
};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Intent, Kind};

fn fixture(src: &str) -> (MemStore, svc_core::Langs, svc_core::Snapshot) {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    (store, langs, snap)
}

#[test]
fn rename_onto_an_existing_sibling_is_refused() {
    let (_, _, snap) = fixture("fn a() {}\nfn b() {}\n");
    let a = lookup_name(&snap, "a").unwrap();
    let err = rename(&snap, a, "b").unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    assert!(rename(&snap, a, "c").is_ok());
}

#[test]
fn rename_to_a_name_used_under_another_parent_is_fine() {
    let (_, _, snap) = fixture("struct S;\nimpl S { fn new() -> S { S } }\nfn make() -> S { S }\n");
    let make = lookup_name(&snap, "make").unwrap();
    assert!(
        rename(&snap, make, "new").is_ok(),
        "different parent, no clash"
    );
}

#[test]
fn add_def_of_a_duplicate_is_refused() {
    let (store, langs, snap) = fixture("fn a() {}\n");
    let err = add_def(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        None,
        1,
        b"fn a() -> u32 { 2 }\n",
        Intent::Feature,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn move_def_into_a_parent_that_has_the_name_is_refused() {
    let (store, langs, snap) = fixture("struct S;\nimpl S { fn go() {} }\nfn go() {}\n");
    let top = lookup_name(&snap, "go").err().map(|e| e.to_string());
    assert!(
        top.is_some_and(|e| e.contains("ambiguous")),
        "two `go`s exist under different parents"
    );
    let imp = snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl)
        .map(|(id, _)| *id)
        .unwrap();
    let free_go = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "go" && r.parent.is_none())
        .map(|(id, _)| *id)
        .unwrap();
    let err = move_def(&store, &langs, &snap, free_go, Some(imp), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn edit_def_may_not_change_kind() {
    let (store, langs, snap) = fixture("fn foo() -> u32 { 1 }\n");
    let foo = lookup_name(&snap, "foo").unwrap();
    let err = edit_def(&store, &langs, &snap, foo, b"const foo: u32 = 1;\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot change"), "{err}");
    let (next, _) = edit_def(&store, &langs, &snap, foo, b"fn foo() -> u32 { 2 }\n").unwrap();
    assert_eq!(next.entities[&foo].kind, Kind::Fn);
}

#[test]
fn snapshot_insert_refuses_a_duplicate_sigkey() {
    let (_, _, snap) = fixture("fn a() {}\n");
    let a = lookup_name(&snap, "a").unwrap();
    let mut rec = snap.entities[&a].clone();
    rec.name = "a".into();
    let mut next = snap.clone();
    let err = next.insert(EntityId::new(), rec).unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn snapshot_insert_refuses_a_missing_file() {
    let (_, _, snap) = fixture("fn a() {}\n");
    let a = lookup_name(&snap, "a").unwrap();
    let mut rec = snap.entities[&a].clone();
    rec.file = RelPath::new("src/gone.rs").unwrap();
    rec.name = "b".into();
    let mut next = snap.clone();
    let err = next.insert(EntityId::new(), rec).unwrap_err().to_string();
    assert!(err.contains("no file record"), "{err}");
}

#[test]
fn delete_refusal_names_the_referrers() {
    use svc_core::engine::{delete, lookup_name};
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(
        RelPath::new("src/lib.rs").unwrap(),
        b"fn a() {}\nfn b() { a() }\nfn c() { a(); b() }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a = lookup_name(&snap, "a").unwrap();
    let err = delete(&snap, &store, a).unwrap_err().to_string();
    assert!(err.contains("b (src/lib.rs"), "{err}");
    assert!(err.contains("c (src/lib.rs"), "{err}");
    assert!(!err.contains("more"), "{err}");
}
