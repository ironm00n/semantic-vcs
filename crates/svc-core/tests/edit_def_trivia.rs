//! `edit-def` with a body that starts at the item must keep `///` / `#[attr]`
//! that extent glued onto the entity. Dropping them was silent: the classifier
//! still said binding-preserving and `svc status` was clean.
use std::collections::BTreeMap;

use svc_core::engine::{edit_def, lookup_name, render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::ObservedClass;

#[test]
fn edit_def_keeps_leading_doc_comments() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = "/// identifier roles\nfn f() { 1 }\nfn g() { f() }\n";
    let mut files = BTreeMap::new();
    files.insert(path.clone(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "f").unwrap();
    let (next, class) = edit_def(&store, &langs, &snap, id, b"fn f() { 2 }").unwrap();
    assert_eq!(class, ObservedClass::BindingPreserving, "body only");
    let out = String::from_utf8(render(&next, &store, &langs, false).unwrap().files[&path].clone())
        .unwrap();
    assert!(
        out.contains("/// identifier roles\nfn f() { 2 }"),
        "doc comment must survive a body-only edit-def:\n{out}"
    );
}

#[test]
fn edit_def_keeps_leading_attributes() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = "#[inline]\nfn f() { 1 }\n";
    let mut files = BTreeMap::new();
    files.insert(path.clone(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "f").unwrap();
    let (next, _) = edit_def(&store, &langs, &snap, id, b"fn f() { 2 }").unwrap();
    let out = String::from_utf8(render(&next, &store, &langs, false).unwrap().files[&path].clone())
        .unwrap();
    assert!(
        out.contains("#[inline]\nfn f() { 2 }"),
        "attribute must survive a body-only edit-def:\n{out}"
    );
}

#[test]
fn edit_def_supplied_docs_replace_the_old_ones() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = "/// old\nfn f() { 1 }\n";
    let mut files = BTreeMap::new();
    files.insert(path.clone(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "f").unwrap();
    let (next, _) = edit_def(&store, &langs, &snap, id, b"/// new\nfn f() { 2 }").unwrap();
    let out = String::from_utf8(render(&next, &store, &langs, false).unwrap().files[&path].clone())
        .unwrap();
    assert!(out.contains("/// new\nfn f() { 2 }"), "{out}");
    assert!(!out.contains("/// old"), "{out}");
}
