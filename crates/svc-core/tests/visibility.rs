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
    assert_eq!(
        chunks.len(),
        1,
        "only the let RHS is a chunks ref: {refs:?}"
    );
    assert_eq!(chunks[0], Slot(0), "RHS must be the parameter, not the let");
}

#[test]
fn js_var_is_visible_before_its_declarator() {
    let src = "function f(){ x = 1; var x; return x; }\n";
    let refs = js_item_refs(src);
    assert_eq!(
        frees_named(&refs, "x"),
        0,
        "pre-declaration x was Free: {refs:?}"
    );
    let xs = locals_named(&refs, "x");
    assert_eq!(xs.len(), 2, "{refs:?}");
    assert_eq!(
        xs[0], xs[1],
        "assignment and return must share the var slot"
    );
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
    assert_eq!(
        frees_named(&refs, "x"),
        1,
        "use after the block is Free: {refs:?}"
    );
}

#[test]
fn js_break_label_is_not_the_same_named_var() {
    let src = "function f(){ var loop = 1; loop: while (true) { break loop; } }\n";
    let refs = js_item_refs(src);
    let value = refs
        .iter()
        .filter(|(n, ident)| n == "loop" && matches!(ident, IdentRef::Local(_, Namespace::Value)))
        .count();
    let labels: Vec<_> = refs.iter().filter(|(n, _)| n == "loop").cloned().collect();
    assert!(
        !labels
            .iter()
            .any(|(_, ident)| matches!(ident, IdentRef::Local(_, Namespace::Value))),
        "break loop must not resolve as the var: {labels:?}"
    );
    assert_eq!(value, 0, "{labels:?}");
}

#[test]
fn self_field_is_not_the_shadowing_local() {
    let src = concat!(
        "struct Bytes { src: Vec<u8> }\n",
        "impl Bytes {\n",
        "    fn reindent(&self) {\n",
        "        let mut src = Vec::with_capacity(self.src.len());\n",
        "        let _ = vec![self.src.len()];\n",
        "        src.push(1);\n",
        "    }\n",
        "}\n",
    );
    let refs = rust_named_refs(src, "function_item", "reindent");
    let srcs: Vec<_> = refs.iter().filter(|(n, _)| n == "src").cloned().collect();
    let locals: Vec<_> = srcs
        .iter()
        .filter(|(_, ident)| matches!(ident, IdentRef::Local(_, Namespace::Value)))
        .collect();
    assert_eq!(
        locals.len(),
        1,
        "self.src (and vec![self.src]) must not be the let: {srcs:?}"
    );
}

fn rust_named_refs(src: &str, kind: &str, name: &str) -> Vec<(String, IdentRef)> {
    let lang = RustLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let item = named_item(tree.root_node(), src.as_bytes(), kind, name);
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
    let labels: Vec<_> = refs.iter().filter(|(n, _)| n == "'a").cloned().collect();
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
    let inside: Vec<_> = refs.iter().filter(|(n, _)| n == "'a").cloned().collect();
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
        .filter(|(n, ident)| n == "loop" && matches!(ident, IdentRef::Local(_, Namespace::Label)))
        .cloned()
        .collect();
    assert!(
        labels.is_empty(),
        "arrow must not see the outer label: {refs:?}"
    );
}

#[test]
fn nested_fn_does_not_see_outer_type_param() {
    let src = "fn outer<T>(x: T) { fn inner(y: T) { let _ = y; } }\n";
    let refs = rust_named_refs(src, "function_item", "inner");
    let ts: Vec<_> = refs.iter().filter(|(n, _)| n == "T").cloned().collect();
    assert!(
        ts.iter()
            .any(|(_, ident)| matches!(ident, IdentRef::Free(_))),
        "inner T should be Free: {refs:?}"
    );
    assert!(
        ts.iter()
            .all(|(_, ident)| !matches!(ident, IdentRef::Local(_, Namespace::Type))),
        "nested fn must not see outer T: {ts:?} from {refs:?}"
    );
}

#[test]
fn impl_method_sees_impl_type_param() {
    let src = "struct Foo;\nimpl<T> Foo { fn m(x: T) {} }\n";
    let refs = rust_named_refs(src, "function_item", "m");
    assert!(
        refs.iter()
            .any(|(n, ident)| n == "T" && matches!(ident, IdentRef::Local(_, Namespace::Type))),
        "impl method must see impl T: {refs:?}"
    );
}

#[test]
fn rust_closure_param_is_local_in_the_body() {
    let src = "fn f() { let _ = |x| x; }\n";
    let refs = rust_item_refs(src);
    assert!(
        refs.iter()
            .any(|(n, ident)| n == "x" && matches!(ident, IdentRef::Local(_, Namespace::Value))),
        "closure body `x` must be the parameter slot: {refs:?}"
    );
    assert!(
        !refs
            .iter()
            .any(|(n, ident)| n == "x" && matches!(ident, IdentRef::Free(_))),
        "closure body `x` must not be Free: {refs:?}"
    );
}

#[test]
fn rust_typed_closure_param_is_a_single_slot() {
    let src = "fn f() { let _ = |x: u32| x; }\n";
    let refs = rust_item_refs(src);
    let xs: Vec<_> = refs.iter().filter(|(n, _)| n == "x").collect();
    assert_eq!(
        xs.len(),
        1,
        "typed closure param must not double-bind: {refs:?}"
    );
    assert!(
        matches!(xs[0].1, IdentRef::Local(_, Namespace::Value)),
        "{refs:?}"
    );
}

#[test]
fn closure_inside_fn_still_sees_type_param() {
    let src = "fn outer<T>(x: T) { let _f = |y: T| y; }\n";
    let refs = rust_item_refs(src);
    assert!(
        refs.iter()
            .any(|(n, ident)| n == "T" && matches!(ident, IdentRef::Local(_, Namespace::Type))),
        "closure is not a nested item; it must see outer T: {refs:?}"
    );
}

#[test]
fn macro_args_are_local_refs() {
    // The compiler resolves `x` inside `foo!(x)`; so must we, or an alpha-rename of the
    // binder leaves the macro argument behind (O9 went red on `vec![node]`).
    let src = "fn f() { let x = 1; foo!(x); let y = x; }\n";
    let refs = rust_item_refs(src);
    let xs = locals_named(&refs, "x");
    assert_eq!(
        xs.len(),
        2,
        "both `foo!(x)` and `let y = x` reference x: {refs:?}"
    );
}

#[test]
fn rust_typed_closure_param_gets_one_slot() {
    // `parameters` (closure role) and `parameter` (pattern role) both reach `src`;
    // it must be bound once or Bytes::new rejects duplicate local_ranges (O10).
    let src = "fn f() { let g = |src: &[u8], n: usize| src.len() + n; }\n";
    let refs = rust_item_refs(src);
    let srcs = locals_named(&refs, "src");
    assert_eq!(srcs.len(), 1, "one reference to src in the body: {refs:?}");
    let ns: Vec<_> = locals_named(&refs, "n");
    assert_eq!(ns.len(), 1, "{refs:?}");
    assert_ne!(srcs[0], ns[0], "distinct params get distinct slots");
}

#[test]
fn rust_if_let_while_let_match_and_for_are_local_slots() {
    let src = r#"fn f(x: Option<u32>, xs: &[u32]) -> u32 {
    if let Some(b) = x { b } else { 0 }
    while let Some(c) = x { return c; }
    match x { Some(d) => d, None => 0 }
    for (i, e) in xs.iter().enumerate() { let _ = (i, e); }
    if let Some(h) = x && h > 1 { h } else { 0 }
}
"#;
    let refs = rust_item_refs(src);
    for name in ["b", "c", "d", "i", "e", "h"] {
        assert!(
            !locals_named(&refs, name).is_empty(),
            "{name} must be a local slot: {refs:?}"
        );
        assert_eq!(
            frees_named(&refs, name),
            0,
            "{name} must not be Free: {refs:?}"
        );
        assert!(
            refs.iter()
                .filter(|(n, _)| n == name)
                .all(|(_, ident)| matches!(ident, IdentRef::Local(_, _))),
            "{name} must not be an Entity: {refs:?}"
        );
    }
}

#[test]
fn rust_match_arm_binder_does_not_leak_to_other_arms() {
    let src = "fn f(x: Option<u32>) -> u32 { match x { Some(a) => a, None => a } }\n";
    let refs = rust_item_refs(src);
    assert_eq!(
        locals_named(&refs, "a").len(),
        1,
        "only the Some arm body is Local: {refs:?}"
    );
    assert_eq!(
        frees_named(&refs, "a"),
        1,
        "the other arm must not see the binder: {refs:?}"
    );
}

#[test]
fn rust_if_let_binder_does_not_leak_to_else_or_after() {
    let src = "fn f(x: Option<u32>) -> u32 { if let Some(a) = x { a } else { a }; a }\n";
    let refs = rust_item_refs(src);
    assert_eq!(
        locals_named(&refs, "a").len(),
        1,
        "only the then-branch is Local: {refs:?}"
    );
    assert_eq!(
        frees_named(&refs, "a"),
        2,
        "else and after the if must not see the binder: {refs:?}"
    );
}

#[test]
fn rust_while_let_binder_does_not_leak_after_the_loop() {
    let src = "fn f(mut x: Option<u32>) -> u32 { while let Some(p) = x { x = None; return p; } p }\n";
    let refs = rust_item_refs(src);
    assert_eq!(
        locals_named(&refs, "p").len(),
        1,
        "only the loop body is Local: {refs:?}"
    );
    assert_eq!(
        frees_named(&refs, "p"),
        1,
        "after the loop must not see the binder: {refs:?}"
    );
}

#[test]
fn rust_or_pattern_binders_share_a_slot() {
    let src = "fn f(r: Result<u32, u32>) -> u32 { match r { Ok(x) | Err(x) => x } }\n";
    let refs = rust_item_refs(src);
    let xs = locals_named(&refs, "x");
    assert_eq!(xs.len(), 1, "{refs:?}");
    assert_eq!(frees_named(&refs, "x"), 0, "{refs:?}");
}

#[test]
fn rust_none_in_a_match_is_not_a_local() {
    let src = "fn f(x: Option<u32>) -> u32 { match x { None => 0, Some(v) => v } }\n";
    let refs = rust_item_refs(src);
    assert!(
        locals_named(&refs, "None").is_empty(),
        "None must not be a binder: {refs:?}"
    );
    assert!(
        refs.iter()
            .filter(|(n, _)| n == "None")
            .all(|(_, ident)| !matches!(ident, IdentRef::Local(_, _))),
        "{refs:?}"
    );
}

#[test]
fn while_let_p_is_not_the_helper_fn_p() {
    let store = MemStore::new();
    let langs = rust_langs();
    let canon = RelPath::new("crates/svc-core/src/canon.rs").unwrap();
    let helper = RelPath::new("crates/svc-core/tests/helpers.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        canon.clone(),
        b"fn syntax_root(mut n: Option<u32>) { while let Some(p) = n { n = p; } }\n".to_vec(),
    );
    files.insert(helper, b"fn p() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let env = env_from_snapshot(&snap);
    let src = "fn syntax_root(mut n: Option<u32>) { while let Some(p) = n { n = p; } }\n";
    let lang = RustLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let item = tree.root_node().named_child(0).expect("fn");
    let res = resolve(item, src.as_bytes(), &lang, &env).unwrap();
    let prefs: Vec<_> = res
        .refs
        .iter()
        .filter(|(r, _)| &src.as_bytes()[r.start as usize..r.end as usize] == b"p")
        .map(|(_, ident)| ident.clone())
        .collect();
    assert!(
        prefs
            .iter()
            .all(|ident| matches!(ident, IdentRef::Local(_, _))),
        "while-let p must not bind to fn p: {prefs:?}"
    );
    assert!(!prefs.is_empty(), "{prefs:?}");
}

#[test]
fn nested_fn_is_not_in_the_file_env() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), b"fn f() { fn g() {} g(); }\nfn h() { g(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let mut env = env_from_snapshot(&snap);
    env.current_file = Some(path);
    assert!(
        env.lookup("g", Namespace::Value).is_none(),
        "nested g must not occupy the file map: {:?}",
        env.lookup("g", Namespace::Value)
    );
    let src = "fn h() { g(); }\n";
    let lang = RustLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let item = tree.root_node().named_child(0).expect("fn");
    let res = resolve(item, src.as_bytes(), &lang, &env).unwrap();
    assert!(
        res.refs.iter().any(|(_, ident)| matches!(ident, IdentRef::Free(_))),
        "sibling g() must be Free: {:?}",
        res.refs
    );
}

#[test]
fn mod_tests_fn_is_not_in_the_file_env() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"#[cfg(test)]\nmod tests {\n    fn g() {}\n    fn t() { g(); }\n}\nfn h() { g(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let mut env = env_from_snapshot(&snap);
    env.current_file = Some(path);
    assert!(
        env.lookup("g", Namespace::Value).is_none(),
        "mod tests g must not occupy the file map: {:?}",
        env.lookup("g", Namespace::Value)
    );
    assert!(
        env.lookup("tests", Namespace::Value).is_some(),
        "the mod itself stays in the file map"
    );
}

#[test]
fn rust_const_block_does_not_see_outer_locals() {
    let src = "fn f() { let k = 1u8; const { let _ = k; } let _ = k; }\n";
    let refs = rust_item_refs(src);
    let ks: Vec<_> = refs
        .iter()
        .filter(|(n, _)| n == "k")
        .map(|(_, ident)| ident)
        .collect();
    assert_eq!(ks.len(), 2, "const-block k and the later use: {refs:?}");
    assert!(
        matches!(ks[0], IdentRef::Free(_)),
        "const block must not capture the local: {refs:?}"
    );
    assert!(
        matches!(ks[1], IdentRef::Local(_, Namespace::Value)),
        "the later use is the local: {refs:?}"
    );
}

#[test]
fn rust_const_block_sees_const_generics() {
    let src = "fn f<const N: usize>() { const { let _ = N; } }\n";
    let refs = rust_item_refs(src);
    assert!(
        refs.iter()
            .any(|(n, ident)| n == "N" && matches!(ident, IdentRef::Local(_, Namespace::Value))),
        "const generic N stays visible in a const block: {refs:?}"
    );
}

#[test]
fn rust_for_label_is_not_visible_in_the_iterator() {
    let src = "fn f() { 'a: for _ in { break 'a; [] } { break 'a; } }\n";
    let refs = rust_item_refs(src);
    let labels: Vec<_> = refs.iter().filter(|(n, _)| n == "'a").cloned().collect();
    assert_eq!(labels.len(), 2, "{refs:?}");
    assert!(
        matches!(labels[0].1, IdentRef::Free(_)),
        "iterator must not see the for-label: {refs:?}"
    );
    assert!(
        matches!(labels[1].1, IdentRef::Local(_, Namespace::Label)),
        "body break is the for-label: {refs:?}"
    );
}

#[test]
fn rust_const_generic_arg_does_not_see_outer_local() {
    let src = "fn f() { let n = 1usize; let _ = [0u8; n]; }\n";
    let refs = rust_item_refs(src);
    let ns: Vec<_> = refs.iter().filter(|(n, _)| n == "n").cloned().collect();
    assert_eq!(ns.len(), 1, "{refs:?}");
    assert!(
        matches!(ns[0].1, IdentRef::Free(_)),
        "array length is a const arg, not the local: {refs:?}"
    );
}

#[test]
fn rust_const_arg_does_not_see_fn_lifetime() {
    let src = "fn f<'a>() { let _ = [0u8; { let _: &'a u8; 1 }]; }\n";
    let refs = rust_item_refs(src);
    let lifetimes: Vec<_> = refs
        .iter()
        .filter(|(n, _)| n == "'a")
        .map(|(_, ident)| ident)
        .collect();
    assert!(
        !lifetimes.is_empty(),
        "expected a use of 'a: {refs:?}"
    );
    assert!(
        lifetimes
            .iter()
            .all(|ident| matches!(ident, IdentRef::Free(_))),
        "const array length must not see the fn lifetime: {refs:?}"
    );
}

#[test]
fn rust_fn_lifetime_still_binds_in_types() {
    let src = "fn f<'a>(x: &'a u8) -> &'a u8 { let _: Vec<&'a u8>; x }\n";
    let refs = rust_item_refs(src);
    let lifetimes: Vec<_> = refs.iter().filter(|(n, _)| n == "'a").cloned().collect();
    assert!(
        lifetimes
            .iter()
            .all(|(_, ident)| matches!(ident, IdentRef::Local(_, Namespace::Lifetime))),
        "ordinary type uses of 'a stay the fn lifetime: {refs:?}"
    );
}

#[test]
fn rust_gen_block_does_not_see_outer_label() {
    let src = "fn f() { 'a: loop { let _ = gen { break 'a; }; } }\n";
    let refs = rust_item_refs(src);
    assert!(
        !refs.iter().any(|(n, ident)| n == "'a"
            && matches!(ident, IdentRef::Local(_, Namespace::Label))),
        "gen block must not see the outer label: {refs:?}"
    );
}

#[test]
fn rust_unbraced_const_generic_arg_does_not_see_outer_local() {
    let src = "fn f() { fn g<const N: usize>() {} let n = 1usize; g::<n>(); }\n";
    let refs = rust_item_refs(src);
    let ns: Vec<_> = refs.iter().filter(|(n, _)| n == "n").cloned().collect();
    assert_eq!(ns.len(), 1, "{refs:?}");
    assert!(
        matches!(ns[0].1, IdentRef::Free(_)),
        "unbraced const generic arg must not see the local: {refs:?}"
    );
}

#[test]
fn rust_hrtb_lifetime_is_a_slot() {
    let src = "fn f() { let _: for<'a> fn(&'a u8); }\n";
    let refs = rust_item_refs(src);
    assert!(
        refs.iter()
            .any(|(n, ident)| n == "'a" && matches!(ident, IdentRef::Local(_, Namespace::Lifetime))),
        "HRTB 'a must be a slot in the fn type: {refs:?}"
    );
}

#[test]
fn rust_hrtb_lifetime_does_not_leak_past_the_fn_type() {
    let src = "fn f() { let _: for<'a> fn(&'a u8); let _: &'a u8; }\n";
    let refs = rust_item_refs(src);
    let lifetimes: Vec<_> = refs.iter().filter(|(n, _)| n == "'a").cloned().collect();
    assert_eq!(lifetimes.len(), 2, "{refs:?}");
    assert!(
        matches!(lifetimes[0].1, IdentRef::Local(_, Namespace::Lifetime)),
        "fn-type use is the HRTB binder: {refs:?}"
    );
    assert!(
        matches!(lifetimes[1].1, IdentRef::Free(_)),
        "a later type must not see the HRTB binder: {refs:?}"
    );
}

#[test]
fn rust_hrtb_trait_bound_lifetime_is_a_slot() {
    let src = "fn f<T: for<'a> Fn(&'a u8)>() {}\n";
    let refs = rust_item_refs(src);
    assert!(
        refs.iter()
            .any(|(n, ident)| n == "'a" && matches!(ident, IdentRef::Local(_, Namespace::Lifetime))),
        "for<'a> Trait must bind 'a in the bound: {refs:?}"
    );
}
