//! Typed ops are the only write path, so a body that does not parse is refused.
//! Ingest of a working copy stays lenient: a checked-in file may be mid-edit.
use std::collections::BTreeMap;

use svc_core::engine::{add_def, edit_def, lookup_name, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Error, Intent};

fn fixture() -> (MemStore, svc_core::Langs, svc_core::Snapshot) {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(
        RelPath::new("src/lib.rs").unwrap(),
        b"fn ok() -> u32 { 1 }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    (store, langs, snap)
}

#[test]
fn edit_def_refuses_a_body_with_syntax_errors() {
    let (store, langs, snap) = fixture();
    let ok = lookup_name(&snap, "ok").unwrap();
    let err = edit_def(
        &store,
        &langs,
        &snap,
        ok,
        b"fn ok( -> u32 { let x = ; 1 }\n",
    )
    .unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err}");
}

#[test]
fn add_def_refuses_a_body_with_syntax_errors() {
    let (store, langs, snap) = fixture();
    let err = add_def(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        None,
        1,
        b"fn broken( {}\n",
        Intent::Feature,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err}");
}

#[test]
fn ingest_of_a_broken_working_copy_still_succeeds() {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(
        RelPath::new("src/lib.rs").unwrap(),
        b"fn ok() {}\nfn broken( {\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    assert!(snap.entities.values().any(|r| r.name == "ok"));
}
