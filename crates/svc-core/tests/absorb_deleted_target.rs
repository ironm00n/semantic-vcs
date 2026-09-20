//! A hand edit that deletes a definition other entities referred to: the re-ingest
//! must resolve those references afresh, not keep them bound to the gone id. It used
//! to seed the resolver with every entity of the previous snapshot, so the references
//! stayed on the deleted one and rendered as `?` — written to disk by the next verb.
use std::collections::BTreeMap;

use svc_core::engine::{render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::{MemStore, Store};

const V1: &str = "fn helper(x: u32) -> u32 { x + 1 }\nfn other() -> u32 { 7 }\nfn caller() -> u32 { helper(other()) }\n";
const V2: &str = "fn helper2(x: u32) -> u32 { x + 1 }\nfn other() -> u32 { 7 }\nfn caller() -> u32 { helper(other()) }\n";

#[test]
fn reingest_after_deleting_a_referenced_definition_renders_the_disk_text() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), V1.as_bytes().to_vec());
    let change = ChangeId::new();
    let s1 = snapshot_files(&store, &langs, &files, None, change).unwrap();
    files.insert(path.clone(), V2.as_bytes().to_vec());
    let s2 = snapshot_files(&store, &langs, &files, Some(&s1), change).unwrap();
    let out = String::from_utf8(render(&s2, &store, &langs, false).unwrap().files[&path].clone()).unwrap();
    assert_eq!(out, V2);
    // And a second ingest of the same bytes is a fixpoint — one absorb converges.
    let s3 = snapshot_files(&store, &langs, &files, Some(&s2), change).unwrap();
    assert!(s3.content_eq(&s2), "second ingest changed the snapshot");
}

#[test]
fn a_reference_whose_target_moved_to_another_file_follows_it() {
    // Deleting `helper` here while another file gains one: the call binds to the new one.
    let store = MemStore::new();
    let langs = rust_langs();
    let a = RelPath::new("src/a.rs").unwrap();
    let b = RelPath::new("src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(a.clone(), V1.as_bytes().to_vec());
    files.insert(b.clone(), "fn unrelated() {}\n".as_bytes().to_vec());
    let change = ChangeId::new();
    let s1 = snapshot_files(&store, &langs, &files, None, change).unwrap();
    files.insert(a.clone(), V2.as_bytes().to_vec());
    files.insert(b.clone(), "fn unrelated() {}\nfn helper(x: u32) -> u32 { x }\n".as_bytes().to_vec());
    let s2 = snapshot_files(&store, &langs, &files, Some(&s1), change).unwrap();
    let rendered = render(&s2, &store, &langs, false).unwrap().files;
    assert_eq!(std::str::from_utf8(&rendered[&a]).unwrap(), V2);
    let helper_b = s2
        .entities
        .iter()
        .find(|(_, r)| r.name == "helper" && r.file == b)
        .map(|(id, _)| *id)
        .unwrap();
    let caller = s2.entities.iter().find(|(_, r)| r.name == "caller").map(|(id, _)| *id).unwrap();
    let content = store.get_content(s2.entities[&caller].content).unwrap();
    assert!(
        content.tokens.iter().any(|t| matches!(t,
            svc_core::Token::Ident(svc_core::IdentRef::Entity(id)) if id == &helper_b)),
        "{:?}",
        content.tokens
    );
}
