//! Visibility variants the role table already emits must change what
//! `resolve_locals` reports. Audit A2: only AfterStmt was interpreted.

use svc_core::content::{IdentRef, Namespace};
use svc_core::engine::{parse, resolve};
use svc_core::ids::Slot;
use svc_core::lang::Env;
use svc_core::{JsLang, RustLang};

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
