//! Canonical token classes: literals are `Lit`, not keywords. The class does not
//! change the hash (O2 compares token streams), but `svc show-def` and any consumer
//! of `Content` read the class.
use svc_core::engine::{canonicalize, extract, parse, resolve};
use svc_core::{Env, Lang, RustLang, Token};

fn tokens(src: &str) -> Vec<Token> {
    let lang = RustLang;
    let tree = parse(src.as_bytes(), &lang).unwrap();
    let raw = extract(&tree, src.as_bytes(), &lang).unwrap();
    let item = tree.root_node().named_child(0).unwrap();
    let res = resolve(item, src.as_bytes(), &lang, &Env::default()).unwrap();
    let _ = &raw;
    canonicalize(item, src.as_bytes(), &res, &[], &Env::default(), &lang)
        .unwrap()
        .tokens
}

#[test]
fn rust_literals_are_lit_tokens() {
    let toks =
        tokens("fn f() -> u32 { let s = \"hi\\n\"; let c = 'c'; if true { 42 } else { 0 } }\n");
    let lits: Vec<String> = toks
        .iter()
        .filter_map(|t| match t {
            Token::Lit(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert!(lits.contains(&"42".to_string()), "{toks:?}");
    assert!(lits.contains(&"0".to_string()), "{toks:?}");
    assert!(lits.contains(&"true".to_string()), "{toks:?}");
    assert!(lits.contains(&"'c'".to_string()), "{toks:?}");
    assert!(
        lits.iter().any(|s| s.contains("hi")),
        "string content is a literal: {toks:?}"
    );
    assert!(
        !toks
            .iter()
            .any(|t| matches!(t, Token::Kw(k) if k.as_ref() == "42")),
        "a number is not a keyword: {toks:?}"
    );
    assert!(
        toks.iter()
            .any(|t| matches!(t, Token::Kw(k) if k.as_ref() == "fn"))
    );
    let _: &dyn Lang = &RustLang;
}
