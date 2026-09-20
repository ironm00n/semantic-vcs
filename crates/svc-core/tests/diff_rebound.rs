//! Same source bytes with a different content hash is a rebind (engine drift or
//! another entity's change), not a hand edit. `svc status` after rebuilding svc
//! used to absorb 141 "edited: binding-changing" entities nobody typed.
use std::collections::BTreeMap;

use svc_core::content::Token;
use svc_core::engine::{diff, lookup_name, rust_langs, snapshot_files, status_report};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Delta, Store};

const SRC: &str = "fn a() {}\nfn b() { a() }\n";

#[test]
fn same_bytes_different_content_is_rebound_not_an_edit() {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), SRC.as_bytes().to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&base, "b").unwrap();
    let old = base.entities[&id].clone();

    let mut next = base.clone();
    let rec = next.entities.get_mut(&id).unwrap();
    let mut content = store.get_content(rec.content).unwrap();
    content.tokens.push(Token::Punct(".".into()));
    rec.content = store.put_content(&content).unwrap();
    assert_eq!(rec.bytes, old.bytes, "the working copy did not change");
    assert_ne!(rec.content, old.content);

    let deltas = diff(&store, &base, &next).unwrap();
    assert!(
        deltas.iter().any(|d| matches!(d, Delta::Rebound(eid, _) if *eid == id)),
        "{deltas:?}"
    );
    assert!(
        deltas.iter().all(|d| !matches!(d, Delta::Edited(_, _))),
        "bytes-equal must not count as an edit: {deltas:?}"
    );

    let rep = status_report(&store, &base, &next).unwrap();
    assert_eq!(rep.semantic, 0, "{:?}", rep.deltas);
    assert_eq!(rep.rebound, 1, "{:?}", rep.deltas);
    assert!(
        rep.summary().contains("rebound, text unchanged"),
        "{}",
        rep.summary()
    );
}

#[test]
fn a_real_byte_edit_is_still_edited() {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), SRC.as_bytes().to_vec());
    let base = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    files.insert(
        RelPath::new("src/lib.rs").unwrap(),
        b"fn a() {}\nfn b() { a(); }\n".to_vec(),
    );
    let next = snapshot_files(&store, &langs, &files, Some(&base), ChangeId::new()).unwrap();
    let deltas = diff(&store, &base, &next).unwrap();
    assert!(
        deltas.iter().any(|d| matches!(d, Delta::Edited(_, _))),
        "{deltas:?}"
    );
    assert!(
        deltas.iter().all(|d| !matches!(d, Delta::Rebound(_, _))),
        "{deltas:?}"
    );
}
