//! `diff`/`status_report` see files, not only entities: an opaque file added,
//! removed or rewritten is a delta (semantic — svc has no language for it), a
//! source file's tail is layout, and a removed entity carries its name.
use std::collections::BTreeMap;

use svc_core::engine::{diff, rust_langs, snapshot_files, status_report};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Delta, Snapshot};

const SRC: &str = "fn a() {}\nfn b() { a() }\n";

fn snapshot_of(store: &MemStore, files: &BTreeMap<RelPath, Vec<u8>>, prev: Option<&Snapshot>) -> Snapshot {
    snapshot_files(store, &rust_langs(), files, prev, ChangeId::new()).unwrap()
}

fn fixture_path(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

#[test]
fn opaque_file_added_changed_and_removed_are_semantic_deltas() {
    let store = MemStore::new();
    let mut files = BTreeMap::new();
    files.insert(fixture_path("src/lib.rs"), SRC.as_bytes().to_vec());
    let base = snapshot_of(&store, &files, None);

    files.insert(fixture_path("Cargo.toml"), b"[package]\nname = \"x\"\n".to_vec());
    let added = snapshot_of(&store, &files, Some(&base));
    let rep = status_report(&store, &base, &added).unwrap();
    assert_eq!(rep.deltas, vec![Delta::FileAdded(fixture_path("Cargo.toml"))]);
    assert_eq!((rep.semantic, rep.layout), (1, 0));

    files.insert(fixture_path("Cargo.toml"), b"[package]\nname = \"y\"\n".to_vec());
    let changed = snapshot_of(&store, &files, Some(&added));
    let rep = status_report(&store, &added, &changed).unwrap();
    assert_eq!(
        rep.deltas,
        vec![Delta::FileTail {
            path: fixture_path("Cargo.toml"),
            whitespace_only: false
        }]
    );
    assert_eq!((rep.semantic, rep.layout), (1, 0));

    files.remove(&fixture_path("Cargo.toml"));
    let removed = snapshot_of(&store, &files, Some(&changed));
    let rep = status_report(&store, &changed, &removed).unwrap();
    assert_eq!(rep.deltas, vec![Delta::FileRemoved(fixture_path("Cargo.toml"))]);
    assert_eq!((rep.semantic, rep.layout), (1, 0));
}

#[test]
fn source_tail_whitespace_is_layout_and_a_removed_entity_keeps_its_name() {
    let store = MemStore::new();
    let mut files = BTreeMap::new();
    files.insert(fixture_path("src/lib.rs"), SRC.as_bytes().to_vec());
    let base = snapshot_of(&store, &files, None);

    files.insert(fixture_path("src/lib.rs"), format!("{SRC}\n\n").into_bytes());
    let tail = snapshot_of(&store, &files, Some(&base));
    let rep = status_report(&store, &base, &tail).unwrap();
    assert_eq!(
        rep.deltas,
        vec![Delta::FileTail {
            path: fixture_path("src/lib.rs"),
            whitespace_only: true
        }],
    );
    assert_eq!((rep.semantic, rep.layout), (0, 1));

    files.insert(fixture_path("src/lib.rs"), b"fn a() {}\n".to_vec());
    let dropped = snapshot_of(&store, &files, Some(&tail));
    let deltas = diff(&store, &tail, &dropped).unwrap();
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, Delta::Removed { name, .. } if name == "b")),
        "{deltas:?}"
    );
}
