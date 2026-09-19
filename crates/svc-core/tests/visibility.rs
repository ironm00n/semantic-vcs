//! Visibility variants the role table already emits must change what
//! `resolve_locals` reports. Audit A2: only AfterStmt was interpreted.

use std::collections::BTreeMap;

use svc_core::content::{IdentRef, Namespace};
use svc_core::engine::{env_from_snapshot, parse, resolve, rust_langs, snapshot_files};
use svc_core::ids::{ChangeId, RelPath, Slot};
use svc_core::lang::Env;
use svc_core::store::MemStore;
use svc_core::{JsLang, Kind, RustLang};

fn rust_item_refs(src: &str) -> Vec<(String, IdentRef)> {
    let lang = RustLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let item = tree.root_node().named_child(0).expect("one item");
    let res = resolve(item, src.as_bytes(), &lang, &Env::default()).unwrap();
    res.refs
        .iter()
        .map(|(r, ident)| {
            (
                String::from_utf8_lossy(&src.as_bytes()[r.start as usize..r.end as usize])
                    .into_owned(),
                ident.clone(),
            )
        })
        .collect()
}

fn js_item_refs(src: &str) -> Vec<(String, IdentRef)> {
    let lang = JsLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let item = tree.root_node().named_child(0).expect("one item");
    let res = resolve(item, src.as_bytes(), &lang, &Env::default()).unwrap();
    res.refs
        .iter()
        .map(|(r, ident)| {
            (
                String::from_utf8_lossy(&src.as_bytes()[r.start as usize..r.end as usize])
                    .into_owned(),
                ident.clone(),
            )
        })
        .collect()
}

fn locals_named(refs: &[(String, IdentRef)], name: &str) -> Vec<Slot> {
    refs.iter()
        .filter_map(|(n, ident)| match ident {
            IdentRef::Local(slot, Namespace::Value) if n == name => Some(*slot),
            _ => None,
        })
        .collect()
}

fn frees_named(refs: &[(String, IdentRef)], name: &str) -> usize {
    refs.iter()
        .filter(|(n, ident)| n == name && matches!(ident, IdentRef::Free(_)))
        .count()
}

#[test]
fn rust_let_rhs_still_sees_the_outer_binding() {
    let src = "fn f(chunks: i32) { let chunks = chunks + 1; }\n";
    let refs = rust_item_refs(src);
    let chunks = locals_named(&refs, "chunks");
    assert_eq!(chunks.len(), 1, "only the let RHS is a chunks ref: {refs:?}");
    assert_eq!(chunks[0], Slot(0), "RHS must be the parameter, not the let");
}

#[test]
fn js_var_is_visible_before_its_declarator() {
    let src = "function f(){ x = 1; var x; return x; }\n";
    let refs = js_item_refs(src);
    assert_eq!(frees_named(&refs, "x"), 0, "pre-declaration x was Free: {refs:?}");
    let xs = locals_named(&refs, "x");
    assert_eq!(xs.len(), 2, "{refs:?}");
    assert_eq!(xs[0], xs[1], "assignment and return must share the var slot");
}

#[test]
fn js_var_hoists_past_a_block() {
    let src = "function f(){ { x = 1; var x; } return x; }\n";
    let refs = js_item_refs(src);
    assert_eq!(frees_named(&refs, "x"), 0, "{refs:?}");
    let xs = locals_named(&refs, "x");
    assert_eq!(xs.len(), 2, "{refs:?}");
    assert_eq!(xs[0], xs[1]);
}

#[test]
fn js_let_is_block_scoped_not_function_scoped() {
    let src = "function f(){ { let x = 1; x; } x; }\n";
    let refs = js_item_refs(src);
    let locals = locals_named(&refs, "x");
    assert_eq!(locals.len(), 1, "inner use is Local: {refs:?}");
    assert_eq!(frees_named(&refs, "x"), 1, "use after the block is Free: {refs:?}");
}

#[test]
fn js_break_label_is_not_the_same_named_var() {
    let src = "function f(){ var loop = 1; loop: while (true) { break loop; } }\n";
    let refs = js_item_refs(src);
    let value = refs
        .iter()
        .filter(|(n, ident)| n == "loop" && matches!(ident, IdentRef::Local(_, Namespace::Value)))
        .count();
    let labels: Vec<_> = refs
        .iter()
        .filter(|(n, _)| n == "loop")
        .cloned()
        .collect();
    assert!(
        !labels
            .iter()
            .any(|(_, ident)| matches!(ident, IdentRef::Local(_, Namespace::Value))),
        "break loop must not resolve as the var: {labels:?}"
    );
    assert_eq!(value, 0, "{labels:?}");
}

fn named_item<'t>(
    root: tree_sitter::Node<'t>,
    src: &[u8],
    kind: &str,
    name: &str,
) -> tree_sitter::Node<'t> {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == kind {
            if let Some(nm) = n.child_by_field_name("name") {
                if &src[nm.start_byte()..nm.end_byte()] == name.as_bytes() {
                    return n;
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    panic!("no {kind} named {name}");
}

#[test]
fn type_position_foo_is_the_struct_not_the_fn() {
    let src = "struct Foo { x: i32 }\nfn Foo() {}\nfn use_it() -> Foo { Foo(); Foo { x: 0 } }\n";
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let struct_id = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.name == "Foo" && rec.kind == Kind::Struct)
        .map(|(id, _)| *id)
        .expect("struct Foo");
    let fn_id = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.name == "Foo" && rec.kind == Kind::Fn)
        .map(|(id, _)| *id)
        .expect("fn Foo");
    assert_ne!(struct_id, fn_id);
    let env = env_from_snapshot(&snap);
    let lang = RustLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let item = named_item(tree.root_node(), src.as_bytes(), "function_item", "use_it");
    let res = resolve(item, src.as_bytes(), &lang, &env).unwrap();
    let foos: Vec<_> = res
        .refs
        .iter()
        .filter(|(r, _)| &src.as_bytes()[r.start as usize..r.end as usize] == b"Foo")
        .map(|(_, ident)| ident.clone())
        .collect();
    let n_struct = foos
        .iter()
        .filter(|t| matches!(t, IdentRef::Entity(id) if *id == struct_id))
        .count();
    let n_fn = foos
        .iter()
        .filter(|t| matches!(t, IdentRef::Entity(id) if *id == fn_id))
        .count();
    assert!(
        n_struct >= 1,
        "return type / struct literal must be the struct: {foos:?}"
    );
    assert!(n_fn >= 1, "Foo() must be the function: {foos:?}");
}

#[test]
fn rust_break_label_resolves_to_the_loop() {
    let src = "fn f() { 'a: loop { break 'a; } }\n";
    let refs = rust_item_refs(src);
    let labels: Vec<_> = refs
        .iter()
        .filter(|(n, _)| n == "'a")
        .cloned()
        .collect();
    assert!(
        labels
            .iter()
            .any(|(_, ident)| matches!(ident, IdentRef::Local(_, Namespace::Label))),
        "break 'a should be the loop label: {refs:?}"
    );
}

#[test]
fn rust_break_label_does_not_enter_a_closure() {
    let src = "fn f() { 'a: loop { let _c = || { break 'a; }; } }\n";
    let refs = rust_item_refs(src);
    let inside: Vec<_> = refs
        .iter()
        .filter(|(n, _)| n == "'a")
        .cloned()
        .collect();
    assert!(
        !inside
            .iter()
            .any(|(_, ident)| matches!(ident, IdentRef::Local(_, Namespace::Label))),
        "closure must not see the outer label: {inside:?} from {refs:?}"
    );
}

#[test]
fn js_break_label_does_not_enter_an_arrow() {
    let src = "function f(){ loop: while (true) { const g = () => { break loop; }; } }\n";
    let refs = js_item_refs(src);
    let labels: Vec<_> = refs
        .iter()
        .filter(|(n, ident)| {
            n == "loop" && matches!(ident, IdentRef::Local(_, Namespace::Label))
        })
        .cloned()
        .collect();
    assert!(
        labels.is_empty(),
        "arrow must not see the outer label: {refs:?}"
    );
}
