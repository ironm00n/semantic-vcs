use svc_core::engine::{canonicalize, parse, resolve};
use svc_core::ids::ContentId;
use svc_core::lang::Env;
use svc_core::RustLang;

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
    assert_eq!(content_of(a), content_of(b), "α-rename of a local must preserve content");
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
