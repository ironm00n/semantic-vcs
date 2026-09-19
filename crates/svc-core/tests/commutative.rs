//! Ordinal shifts under a commutative parent are layout, not Relocated.

use std::collections::BTreeMap;

use svc_core::delta::Delta;
use svc_core::engine::{diff, merge, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::snapshot::AttrValue;
use svc_core::store::{MemStore, Store};
use svc_core::Conflict;

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

#[test]
fn reordering_fns_does_not_conflict_on_ordinal() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), b"fn a() {}\nfn b() {}\nfn c() {}\n".to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    files.insert(path.clone(), b"fn b() {}\nfn a() {}\nfn c() {}\n".to_vec());
    let a = snapshot_files(&store, &langs, &files, Some(&base), ChangeId::new()).unwrap();
    files.insert(path, b"fn a() {}\nfn c() {}\nfn b() {}\n".to_vec());
    let b = snapshot_files(&store, &langs, &files, Some(&base), ChangeId::new()).unwrap();
    let base_id = store.put_snapshot(&base).unwrap();
    let a_id = store.put_snapshot(&a).unwrap();
    let b_id = store.put_snapshot(&b).unwrap();
    let merged = merge(&store, &langs, base_id, a_id, b_id).unwrap();
    let ordinal = merged.conflicts.iter().any(|c| {
        matches!(
            c,
            Conflict::Attr {
                sides,
                ..
            } if sides.adds().any(|v| matches!(v, AttrValue::Ordinal(_)))
        )
    });
    assert!(
        !ordinal,
        "file-root fn order is layout: {:?}",
        merged.conflicts
    );
}
