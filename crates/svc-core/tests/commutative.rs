//! Ordinal shifts under a commutative parent are layout, not Relocated.

use std::collections::BTreeMap;

use svc_core::delta::Delta;
use svc_core::engine::{diff, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;

#[test]
fn adding_a_fn_does_not_relocate_sibling_fns() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), b"fn a() {}\nfn b() {}\n".to_vec());
    let prev = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    files.insert(path, b"fn c() {}\nfn a() {}\nfn b() {}\n".to_vec());
    let next = snapshot_files(&store, &langs, &files, Some(&prev), ChangeId::new()).unwrap();
    let deltas = diff(&prev, &next);
    assert!(
        deltas.iter().any(|d| matches!(d, Delta::Added(_))),
        "c should be added: {deltas:?}"
    );
    assert!(
        deltas.iter().all(|d| !matches!(d, Delta::Relocated { .. })),
        "file-root fns are commutative: {deltas:?}"
    );
}

#[test]
fn adding_a_fn_still_relocates_a_macro_sibling() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"macro_rules! m { () => {} }\nfn a() {}\n".to_vec(),
    );
    let prev = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    files.insert(
        path,
        b"fn b() {}\nmacro_rules! m { () => {} }\nfn a() {}\n".to_vec(),
    );
    let next = snapshot_files(&store, &langs, &files, Some(&prev), ChangeId::new()).unwrap();
    let deltas = diff(&prev, &next);
    assert!(
        deltas.iter().any(|d| matches!(d, Delta::Relocated { .. })),
        "macros are excepted from file-root commutativity: {deltas:?}"
    );
}
