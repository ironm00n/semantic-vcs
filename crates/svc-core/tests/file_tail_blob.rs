//! A file svc has no language for, and the bytes after a source file's last entity, live
//! in the store as one content-addressed blob per distinct content — not inline in every
//! snapshot. Two snapshots with the same Cargo.lock share the id; the snapshot's own
//! encoding does not grow with the file.
use std::collections::BTreeMap;

use svc_core::engine::{render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;

#[test]
fn opaque_file_bytes_are_one_shared_blob_outside_the_snapshot() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lock = RelPath::new("Cargo.lock").unwrap();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let big = vec![b'x'; 1 << 20];
    let mut files = BTreeMap::new();
    files.insert(lock.clone(), big.clone());
    files.insert(lib.clone(), b"fn a() {}\n".to_vec());
    let change = ChangeId::new();
    let s1 = snapshot_files(&store, &langs, &files, None, change).unwrap();
    files.insert(lib.clone(), b"fn a() {}\nfn b() {}\n".to_vec());
    let s2 = snapshot_files(&store, &langs, &files, Some(&s1), change).unwrap();

    let id1 = s1.files[&lock].trailing.expect("a tail");
    assert_eq!(s2.files[&lock].trailing, Some(id1), "same bytes, same blob");
    let encoded = postcard::to_allocvec(&s2).unwrap();
    assert!(encoded.len() < 64 * 1024, "snapshot carries the 1 MiB file inline: {} bytes", encoded.len());
    assert_eq!(render(&s2, &store, &langs, false).unwrap().files[&lock], big);
    assert_eq!(s2.files[&lib].tail(&store).unwrap(), b"\n", "the source tail is the newline after the last item");
}
