use svc_core::RustLang;
use svc_core::engine::{canonicalize, parse, resolve};
use svc_core::ids::ContentId;
use svc_core::lang::Env;

fn content_of(src: &str) -> ContentId {
    let lang = RustLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let item = tree.root_node().child(0).expect("one item");
    let res = resolve(item, src.as_bytes(), &lang, &Env::default()).unwrap();
    let c = canonicalize(item, src.as_bytes(), &res, &[], &Env::default(), &lang).unwrap();
    c.id()
}

#[test]
fn o2_alpha_rename_param() {
    let a = "fn parse(s: &str) -> usize { s.len() }\n";
    let b = "fn parse(input: &str) -> usize { input.len() }\n";
    assert_eq!(
        content_of(a),
        content_of(b),
        "α-rename of a local must preserve content"
    );
}

#[test]
fn o2_rename_is_not_free_name() {
    let a = "fn parse(s: &str) -> usize { s.len() }\n";
    let b = "fn parse(s: &str) -> usize { s.len() + 1 }\n";
    assert_ne!(content_of(a), content_of(b));
}

#[test]
fn o2_alpha_rename_closure_param() {
    let a = "fn f() { let _ = |s| s.len(); }\n";
    let b = "fn f() { let _ = |x| x.len(); }\n";
    assert_eq!(
        content_of(a),
        content_of(b),
        "α-rename of a closure parameter must preserve content"
    );
}

#[test]
fn o2_alpha_rename_if_let_while_let_match_and_for() {
    let a = r#"fn f(x: Option<u32>, xs: &[u32]) -> u32 {
    if let Some(b) = x { return b; }
    while let Some(c) = x { return c; }
    match x { Some(d) => d, None => 0 }
    for (i, e) in xs.iter().enumerate() { let _ = (i, e); }
    if let Some(h) = x && h > 1 { h } else { 0 }
}
"#;
    let b = r#"fn f(x: Option<u32>, xs: &[u32]) -> u32 {
    if let Some(p) = x { return p; }
    while let Some(q) = x { return q; }
    match x { Some(r) => r, None => 0 }
    for (j, k) in xs.iter().enumerate() { let _ = (j, k); }
    if let Some(s) = x && s > 1 { s } else { 0 }
}
"#;
    assert_eq!(
        content_of(a),
        content_of(b),
        "α-rename of if-let / while-let / match / for / let-chain binders must preserve content"
    );
}

#[test]
fn o2_alpha_rename_or_pattern() {
    let a = "fn f(r: Result<u32, u32>) -> u32 { match r { Ok(x) | Err(x) => x } }\n";
    let b = "fn f(r: Result<u32, u32>) -> u32 { match r { Ok(y) | Err(y) => y } }\n";
    assert_eq!(
        content_of(a),
        content_of(b),
        "or-pattern binders with the same name are one slot"
    );
}
