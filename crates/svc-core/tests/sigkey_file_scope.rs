//! File-level items are unique per file, not per repository: two modules may each
//! define `fn hex32` and both `use serde::Serialize;`. Matching, merge unification
//! and add/add detection all key on that scope.
use std::collections::BTreeMap;

use svc_core::engine::{edit_def, lookup_name, merge, rename, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::{MemStore, Store};
use svc_core::{Conflict, Kind};

fn two_files() -> BTreeMap<RelPath, Vec<u8>> {
    let mut files = BTreeMap::new();
    files.insert(
        RelPath::new("src/a.rs").unwrap(),
        b"use serde::Serialize;\nfn hex32(b: &[u8]) -> String { format!(\"{b:?}\") }\nfn only_a() -> u32 { 1 }\n".to_vec(),
    );
    files.insert(
        RelPath::new("src/b.rs").unwrap(),
        b"use serde::Serialize;\nfn hex32(b: &[u8]) -> String { format!(\"{b:x?}\") }\nfn only_b() -> u32 { 2 }\n".to_vec(),
    );
    files
}

#[test]
fn same_name_in_two_files_keeps_ids_across_reingest() {
    let store = MemStore::new();
    let langs = rust_langs();
    let files = two_files();
    let s = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let ids: BTreeMap<_, _> = s
        .entities
        .iter()
        .filter(|(_, r)| r.name == "hex32")
        .map(|(id, r)| (r.file.clone(), *id))
        .collect();
    assert_eq!(ids.len(), 2);
    // Re-ingest with b.rs changed above hex32 so file order and content both move.
    let mut again = files.clone();
    again.insert(
        RelPath::new("src/b.rs").unwrap(),
        b"//! b\nuse serde::Serialize;\nfn hex32(b: &[u8]) -> String { format!(\"{b:x?}\") }\nfn only_b() -> u32 { 2 }\n".to_vec(),
    );
    let n = snapshot_files(&store, &langs, &again, Some(&s), s.change).unwrap();
    for (file, id) in &ids {
        let rec = &n.entities[id];
        assert_eq!(&rec.file, file, "hex32 in {file} kept its id");
    }
}

#[test]
fn unrelated_edits_in_two_files_merge_without_add_add() {
    let store = MemStore::new();
    let langs = rust_langs();
    let base = snapshot_files(&store, &langs, &two_files(), None, ChangeId::new()).unwrap();
    let only_a = lookup_name(&base, "only_a").unwrap();
    let only_b = lookup_name(&base, "only_b").unwrap();
    let (a, _) = edit_def(
        &store,
        &langs,
        &base,
        only_a,
        b"fn only_a() -> u32 { 10 }\n",
    )
    .unwrap();
    let (b, _) = edit_def(
        &store,
        &langs,
        &base,
        only_b,
        b"fn only_b() -> u32 { 20 }\n",
    )
    .unwrap();
    let (bi, ai, bbi) = (
        store.put_snapshot(&base).unwrap(),
        store.put_snapshot(&a).unwrap(),
        store.put_snapshot(&b).unwrap(),
    );
    let merged = merge(&store, &langs, bi, ai, bbi).unwrap();
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
    assert_eq!(
        merged
            .entities
            .values()
            .filter(|r| r.name == "hex32")
            .count(),
        2
    );
    assert_eq!(
        merged
            .entities
            .values()
            .filter(|r| r.kind == Kind::Opaque)
            .count(),
        2
    );
}

#[test]
fn rename_may_reuse_a_name_taken_in_another_file() {
    let store = MemStore::new();
    let langs = rust_langs();
    let s = snapshot_files(&store, &langs, &two_files(), None, ChangeId::new()).unwrap();
    let only_a = lookup_name(&s, "only_a").unwrap();
    assert!(
        rename(&store, &s, only_a, "only_b").is_ok(),
        "only_b lives in b.rs; a.rs is free to use it"
    );
    let only_b = lookup_name(&s, "only_b").unwrap();
    let hex_in_b = s
        .entities
        .iter()
        .find(|(_, r)| r.name == "hex32" && r.file.as_str() == "src/b.rs")
        .map(|(id, _)| *id)
        .unwrap();
    let err = rename(&store, &s, only_b, "hex32").unwrap_err().to_string();
    assert!(
        err.contains("already exists") && err.contains(&hex_in_b.short()),
        "{err}"
    );
    let _ = Conflict::AddAdd {
        key: s.entities[&only_a].sig_key(),
        a: only_a,
        b: only_a,
    };
}
