//! `svc status` reports an entity that moved to another file as a layout delta.
//! A stub used to drop every `Relocated` before it reached the report.
use std::collections::BTreeMap;

use svc_core::engine::{lookup_name, relocate, rust_langs, snapshot_files, status_report};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::Delta;

#[test]
fn moving_an_entity_to_another_file_is_a_layout_delta() {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/a.rs").unwrap(), b"fn a() {}\n".to_vec());
    files.insert(RelPath::new("src/b.rs").unwrap(), b"fn b() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a = lookup_name(&snap, "a").unwrap();
    let next = relocate(&snap, a, RelPath::new("src/b.rs").unwrap(), 1).unwrap();
    let rep = status_report(&snap, &next);
    assert!(
        rep.deltas.iter().any(|d| matches!(d, Delta::Relocated { id, .. } if *id == a)),
        "{:?}",
        rep.deltas
    );
    assert_eq!(rep.semantic, 0, "{:?}", rep.deltas);
    assert_eq!(rep.layout, 1, "{:?}", rep.deltas);
}
