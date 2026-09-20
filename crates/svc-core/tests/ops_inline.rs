//! `inline` must rewrite the unique call and drop the callee. Deleting through
//! a remaining Name hole used to refuse the only use the verb exists to remove.
use std::collections::BTreeMap;

use svc_core::Snapshot;
use svc_core::engine::{inline, lookup_name, render, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;

fn fixture(src: &str) -> (MemStore, svc_core::Langs, Snapshot) {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    (store, langs, snap)
}

fn rendered(store: &MemStore, snap: &Snapshot) -> String {
    let langs = rust_langs();
    let out = render(snap, store, &langs, false).unwrap();
    String::from_utf8(out.files.into_values().next().unwrap()).unwrap()
}

#[test]
fn inline_splices_the_call_and_drops_the_fn() {
    let src = r#"fn helper(x: i32) -> i32 {
    x + 1
}

fn use_it() {
    let y = helper(3);
    let _ = y;
}
"#;
    let (store, langs, snap) = fixture(src);
    let helper = lookup_name(&snap, "helper").unwrap();
    let next = inline(&store, &langs, &snap, helper).unwrap();
    assert!(!next.entities.contains_key(&helper));
    assert!(lookup_name(&next, "use_it").is_ok());
    let src = rendered(&store, &next);
    assert!(!src.contains("fn helper"), "{src}");
    assert!(!src.contains("helper("), "{src}");
    assert!(src.contains("let x = 3"), "{src}");
    assert!(src.contains("x + 1"), "{src}");
    assert!(src.contains("fn use_it"), "{src}");
}

#[test]
fn inline_refuses_two_call_sites() {
    let src = r#"fn helper() {}
fn a() { helper(); }
fn b() { helper(); }
"#;
    let (store, langs, snap) = fixture(src);
    let helper = lookup_name(&snap, "helper").unwrap();
    let err = inline(&store, &langs, &snap, helper)
        .unwrap_err()
        .to_string();
    assert!(err.contains("found 2"), "{err}");
}

#[test]
fn inline_refuses_zero_uses() {
    let src = r#"fn helper() {}
fn other() {}
"#;
    let (store, langs, snap) = fixture(src);
    let helper = lookup_name(&snap, "helper").unwrap();
    let err = inline(&store, &langs, &snap, helper)
        .unwrap_err()
        .to_string();
    assert!(err.contains("found 0"), "{err}");
}

#[test]
fn inline_of_a_nested_item_into_its_parent_is_refused() {
    let src = r#"fn outer() {
    fn inner() {}
    inner();
}
"#;
    let (store, langs, snap) = fixture(src);
    let inner = lookup_name(&snap, "inner").unwrap();
    let err = inline(&store, &langs, &snap, inner)
        .unwrap_err()
        .to_string();
    assert!(err.contains("extract first"), "{err}");
}

#[test]
fn inline_refuses_while_a_use_line_imports_the_callee() {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(
        RelPath::new("src/a.rs").unwrap(),
        b"pub fn helper(x: u32) -> u32 { x + 1 }\n".to_vec(),
    );
    files.insert(
        RelPath::new("src/lib.rs").unwrap(),
        b"mod a;\nuse crate::a::{helper};\nfn caller() -> u32 { helper(1) }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let helper = lookup_name(&snap, "helper").unwrap();
    let err = inline(&store, &langs, &snap, helper).unwrap_err().to_string();
    assert!(err.contains("imported by `use crate::a::{helper};` in src/lib.rs"), "{err}");
}
