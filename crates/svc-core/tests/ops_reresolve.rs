//! Move and extract re-resolve the item in the destination tree. A method that
//! inherited `impl<T>` must see `T` as free once it is a file-root fn.
use std::collections::BTreeMap;

use svc_core::content::{IdentRef, Namespace, Token};
use svc_core::engine::{extract_hoist, lookup_name, move_def, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::{MemStore, Store};
use svc_core::{Kind, Snapshot};

const SRC: &str = r#"struct Foo;
impl<T> Foo {
    fn method(x: T) {
        let _ = x;
    }
}

fn other() {}
"#;

fn fixture() -> (MemStore, svc_core::Langs, Snapshot) {
    let store = MemStore::new();
    let langs = rust_langs();
    let mut files = BTreeMap::new();
    files.insert(RelPath::new("src/lib.rs").unwrap(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    (store, langs, snap)
}

fn tokens(store: &MemStore, snap: &Snapshot, id: svc_core::ids::EntityId) -> Vec<Token> {
    store
        .get_content(snap.entities[&id].content)
        .unwrap()
        .tokens
}

fn has_free_type(
    store: &MemStore,
    snap: &Snapshot,
    id: svc_core::ids::EntityId,
    name: &str,
) -> bool {
    tokens(store, snap, id)
        .iter()
        .any(|t| matches!(t, Token::Ident(IdentRef::Free(n)) if n.as_ref() == name))
}

fn has_local_type(store: &MemStore, snap: &Snapshot, id: svc_core::ids::EntityId) -> bool {
    tokens(store, snap, id)
        .iter()
        .any(|t| matches!(t, Token::Ident(IdentRef::Local(_, Namespace::Type))))
}

#[test]
fn extract_drops_impl_generics_the_method_inherited() {
    let (store, langs, snap) = fixture();
    let method = lookup_name(&snap, "method").unwrap();
    assert!(
        has_local_type(&store, &snap, method),
        "method should inherit impl<T> before extract: {:?}",
        tokens(&store, &snap, method)
    );
    assert!(!has_free_type(&store, &snap, method, "T"));
    let next = extract_hoist(&store, &langs, &snap, method, None, 2).unwrap();
    assert!(next.entities[&method].parent.is_none());
    assert!(
        has_free_type(&store, &next, method, "T"),
        "extracted method must not keep impl<T>: {:?}",
        tokens(&store, &next, method)
    );
    assert!(!has_local_type(&store, &next, method));
}

#[test]
fn move_into_an_impl_inherits_its_generics() {
    let (store, langs, snap) = fixture();
    let method = lookup_name(&snap, "method").unwrap();
    let imp = snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::Impl)
        .map(|(id, _)| *id)
        .unwrap();
    let hoisted = extract_hoist(&store, &langs, &snap, method, None, 2).unwrap();
    assert!(has_free_type(&store, &hoisted, method, "T"));
    let back = move_def(&store, &langs, &hoisted, method, Some(imp), Some(0)).unwrap();
    assert_eq!(back.entities[&method].parent, Some(imp));
    assert!(
        has_local_type(&store, &back, method),
        "moving back under impl<T> should rebind T: {:?}",
        tokens(&store, &back, method)
    );
    assert!(!has_free_type(&store, &back, method, "T"));
}
