//! `add-def --ordinal n` / `relocate … --ordinal n` at a file root place the entity at
//! that position: a root already at `n` and everything after it move down one, so
//! render order is the asked-for order, not a tie broken by id.
//! The blank line between roots lives in the second one, so an item moved to or from the
//! front has its leading newlines set to what its new position needs.
use std::collections::BTreeMap;

use svc_core::engine::{add_def, lookup_name, relocate, render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::store::MemStore;
use svc_core::Intent;

const SRC: &str = "fn a() {}\n\nfn b() {}\n\nfn c() {}\n";

fn order(out: &str) -> Vec<&str> {
    out.lines()
        .filter_map(|l| l.strip_prefix("fn ").and_then(|r| r.split('(').next()))
        .collect()
}

#[test]
fn add_def_at_a_taken_root_ordinal_shifts_the_rest_down() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let b = lookup_name(&snap, "b").unwrap();
    assert_eq!(snap.entities[&b].ordinal, 1);

    let next = add_def(&store, &langs, &snap, EntityId::new(), None, 1, b"fn n() {}", Intent::Feature).unwrap();
    let out = String::from_utf8(render(&next, &store, &langs, false).unwrap().files[&path].clone()).unwrap();
    assert_eq!(order(&out), ["a", "n", "b", "c"], "{out}");
    let ords: Vec<u32> = next.file_roots(&path).iter().map(|id| next.entities[id].ordinal).collect();
    assert!(ords.windows(2).all(|w| w[0] < w[1]), "ordinals must be distinct and ordered: {ords:?}");
}

#[test]
fn relocate_to_a_taken_root_ordinal_shifts_the_rest_down() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let c = lookup_name(&snap, "c").unwrap();

    let next = relocate(&snap, &store, c, path.clone(), 0).unwrap();
    let names: Vec<&str> = next
        .file_roots(&path)
        .iter()
        .map(|id| next.entities[id].name.as_str())
        .collect();
    assert_eq!(names, ["c", "a", "b"]);
    let out = String::from_utf8(render(&next, &store, &langs, false).unwrap().files[&path].clone()).unwrap();
    assert!(out.find("fn c()").unwrap() < out.find("fn a()").unwrap(), "{out}");
}

#[test]
fn relocating_to_the_front_and_back_keeps_items_separated() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a = lookup_name(&snap, "a").unwrap();
    let c = lookup_name(&snap, "c").unwrap();

    let front = relocate(&snap, &store, c, path.clone(), 0).unwrap();
    let out = String::from_utf8(render(&front, &store, &langs, false).unwrap().files[&path].clone()).unwrap();
    assert_eq!(out, "fn c() {}\n\nfn a() {}\n\nfn b() {}\n");

    let back = relocate(&snap, &store, a, path.clone(), 3).unwrap();
    let out = String::from_utf8(render(&back, &store, &langs, false).unwrap().files[&path].clone()).unwrap();
    assert_eq!(out, "fn b() {}\n\nfn c() {}\n\nfn a() {}\n");

    let reparsed = snapshot_files(
        &store,
        &langs,
        &BTreeMap::from([(path.clone(), out.into_bytes())]),
        Some(&back),
        ChangeId::new(),
    )
    .unwrap();
    assert_eq!(reparsed.entities.len(), 3);
}
