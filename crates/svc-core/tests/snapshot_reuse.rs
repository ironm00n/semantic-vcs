//! An absorb of one file keeps the other files' records instead of re-materializing them,
//! and that shortcut is invisible: the snapshot it yields is the one a full pass yields,
//! entity for entity, whether the edit is trivia, a body, a new definition or a rename.

use std::collections::{BTreeMap, BTreeSet};

use svc_core::engine::{rust_langs, snapshot_files, snapshot_files_reusing};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;

const A: &str = "pub fn helper(x: u32) -> u32 { x + 1 }\npub fn other() -> u32 { 2 }\n";
const B: &str = "use crate::a::helper;\npub fn caller() -> u32 { helper(other_local()) }\nfn other_local() -> u32 { 3 }\n";

fn tree(a: &str, b: &str) -> BTreeMap<RelPath, Vec<u8>> {
    BTreeMap::from([
        (RelPath::new("src/a.rs").unwrap(), a.as_bytes().to_vec()),
        (RelPath::new("src/b.rs").unwrap(), b.as_bytes().to_vec()),
    ])
}

/// Both passes from the same `prev`, `b.rs` untouched: the reusing one must agree.
fn agree(a: &str, label: &str) {
    let store = MemStore::new();
    let langs = rust_langs();
    let change = ChangeId::new();
    let prev = snapshot_files(&store, &langs, &tree(A, B), None, change).unwrap();
    let next = tree(a, B);
    let unchanged = BTreeSet::from([RelPath::new("src/b.rs").unwrap()]);
    let full = snapshot_files(&store, &langs, &next, Some(&prev), change).unwrap();
    let reused = snapshot_files_reusing(&store, &langs, &next, Some(&prev), change, &unchanged).unwrap();
    // A definition new to this pass gets a fresh id each time; everything else must match.
    let shape = |s: &svc_core::Snapshot| {
        let mut v: Vec<_> = s
            .entities
            .values()
            .map(|r| (r.file.clone(), r.name.clone(), r.kind, r.ordinal, r.parent.is_some(), r.content, r.bytes))
            .collect();
        v.sort();
        v
    };
    assert_eq!(shape(&reused), shape(&full), "{label}: entities differ");
    for (id, r) in &prev.entities {
        assert_eq!(reused.entities.get(id).map(|x| &x.name), full.entities.get(id).map(|x| &x.name), "{label}: {} keeps or loses its id in both", r.name);
    }
    assert_eq!(reused.files, full.files, "{label}: file records differ");
}

#[test]
fn reuse_yields_the_full_pass_snapshot() {
    agree("// a comment\n// on top\npub fn helper(x: u32) -> u32 { x + 1 }\npub fn other() -> u32 { 2 }\n", "trivia only");
    agree("pub fn helper(x: u32) -> u32 { x + 2 }\npub fn other() -> u32 { 2 }\n", "a body");
    agree("pub fn helper(x: u32) -> u32 { x + 1 }\npub fn other() -> u32 { 2 }\npub fn added() {}\n", "a new definition");
    agree("pub fn helper_renamed(x: u32) -> u32 { x + 1 }\npub fn other() -> u32 { 2 }\n", "a rename by hand");
    agree("pub fn other() -> u32 { 2 }\n", "a deleted definition");
}

#[test]
fn an_unchanged_file_keeps_its_records_when_nothing_was_declared_or_undeclared() {
    let store = MemStore::new();
    let langs = rust_langs();
    let change = ChangeId::new();
    let prev = snapshot_files(&store, &langs, &tree(A, B), None, change).unwrap();
    let b = RelPath::new("src/b.rs").unwrap();
    let before: BTreeMap<_, _> = prev.entities.iter().filter(|(_, r)| r.file == b).collect();
    let next = tree("pub fn helper(x: u32) -> u32 { x * 2 }\npub fn other() -> u32 { 2 }\n", B);
    let reused = snapshot_files_reusing(&store, &langs, &next, Some(&prev), change, &BTreeSet::from([b.clone()])).unwrap();
    let after: BTreeMap<_, _> = reused.entities.iter().filter(|(_, r)| r.file == b).collect();
    assert_eq!(before, after, "b.rs keeps the same ids, content and bytes");
    assert_eq!(prev.files[&b], reused.files[&b]);
}
