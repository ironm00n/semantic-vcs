//! lang-js lane: entity extraction, Kind refinement, roles, round-trip.
//! Binding data: `facts/js-binding-tables.md`; Kind variants: DECISIONS §25.

use svc_core::engine::{extract, ingest_file, render};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::lang::{Env, Lang, Langs, Locator, Role, Visibility};
use svc_core::Namespace;
use svc_core::{js_kind, JsLang, Kind};

fn parse(src: &str) -> tree_sitter::Tree {
    let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    parser.parse(src, None).unwrap()
}

fn find<'a>(root: tree_sitter::Node<'a>, kind: &str) -> Vec<tree_sitter::Node<'a>> {
    let mut out = vec![];
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == kind {
            out.push(n);
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    out
}

fn find_text<'a>(root: tree_sitter::Node<'a>, kind: &str, text: &str, src: &[u8]) -> tree_sitter::Node<'a> {
    find(root, kind)
        .into_iter()
        .find(|n| &src[n.start_byte()..n.end_byte()] == text.as_bytes())
        .unwrap_or_else(|| panic!("no {kind} {text:?}"))
}

fn roles_of(src: &str, kind: &str, field: Option<&str>) -> Vec<Role> {
    let tree = parse(src);
    let n = find(tree.root_node(), kind).pop().expect("node kind present");
    JsLang.roles(n, field, src.as_bytes(), &Env::default())
}

const TWIN: &str = r#"function read(path) {
  return path;
}

function parse(s) {
  return { path: s, retries: 0 };
}

function validate(c) {
  return c.retries;
}

class Config {
  get path() {
    return this._path;
  }
  set path(v) {
    this._path = v;
  }
  static load(p) {
    return parse(read(p));
  }
}

function main() {
  const c = parse("x");
  return c.path;
}

export { parse };
"#;

#[test]
fn js_extract_roots_and_names() {
    let tree = parse(TWIN);
    let raw = extract(&tree, TWIN.as_bytes(), &JsLang).unwrap();
    let roots: Vec<&str> = raw
        .iter()
        .filter(|e| e.parent_idx.is_none())
        .map(|e| e.name.as_str())
        .collect();
    assert!(roots.contains(&"read"));
    assert!(roots.contains(&"parse"));
    assert!(roots.contains(&"validate"));
    assert!(roots.contains(&"Config"));
    assert!(roots.contains(&"main"));
    // `export { parse };` is transparent: no entity of its own.
    assert!(!roots.iter().any(|n| n.contains("export")));
    // Class members nest under Config.
    let class = raw.iter().find(|e| e.name == "Config").unwrap();
    let class_idx = raw.iter().position(|e| e.name == "Config").unwrap();
    let mut members: Vec<&str> = raw
        .iter()
        .filter(|e| e.parent_idx == Some(class_idx))
        .map(|e| e.name.as_str())
        .collect();
    members.sort_unstable();
    assert_eq!(members, ["load", "path", "path"]);
    assert_eq!(class.kind, Kind::JsClass);
}

#[test]
fn js_kind_matrix() {
    let src = "class C { get p() {} set p(v) {} static m() {} m() {} get() {} static get s() {} static set s(v) {} static x = 1; y = 2; }";
    let tree = parse(src);
    let s = src.as_bytes();
    let by_name = |name: &str| -> Vec<Kind> {
        let mut kinds: Vec<Kind> = find(tree.root_node(), "method_definition")
            .into_iter()
            .filter(|m| {
                m.child_by_field_name("name")
                    .is_some_and(|n| &s[n.start_byte()..n.end_byte()] == name.as_bytes())
            })
            .map(|m| js_kind(m, s).unwrap())
            .collect();
        kinds.sort();
        kinds
    };
    assert_eq!(by_name("p"), [Kind::JsGetter, Kind::JsSetter]);
    assert_eq!(by_name("m"), [Kind::JsMethod, Kind::JsStaticMethod]); // sorted
    // A method *named* `get` is not an accessor (no anonymous `get` token).
    assert_eq!(by_name("get"), [Kind::JsMethod]);
    assert_eq!(by_name("s"), [Kind::JsGetter, Kind::JsSetter]);
    let fields = |prop: &str| -> Kind {
        let f = find(tree.root_node(), "field_definition")
            .into_iter()
            .find(|f| {
                f.child_by_field_name("property")
                    .is_some_and(|n| &s[n.start_byte()..n.end_byte()] == prop.as_bytes())
            })
            .unwrap();
        js_kind(f, s).unwrap()
    };
    assert_eq!(fields("x"), Kind::JsStaticField);
    assert_eq!(fields("y"), Kind::JsField);

    let decl = "const f = x => x; const g = function h() {}; const n = 1;";
    let t2 = parse(decl);
    let mut kinds: Vec<Kind> = find(t2.root_node(), "variable_declarator")
        .into_iter()
        .map(|d| js_kind(d, decl.as_bytes()).unwrap())
        .collect();
    kinds.sort();
    assert_eq!(kinds, [Kind::JsFunction, Kind::JsFunction, Kind::JsDeclarator]);
}

#[test]
fn js_roles_binders() {
    // `const` declarator: lexical binder on the name field.
    assert!(roles_of("const k = 1;", "variable_declarator", None).contains(&Role::Binder {
        namespace: Namespace::Value,
        visibility: Visibility::Whole,
        locator: Locator::Field("name"),
    }));
    // `var` declarator hoists.
    assert!(roles_of("var k = 1;", "variable_declarator", None).contains(&Role::Binder {
        namespace: Namespace::Value,
        visibility: Visibility::Hoisted,
        locator: Locator::Field("name"),
    }));
    // Top-level function declaration hoists; nested block one does not (T10).
    let top = roles_of("function f() {}", "function_declaration", None);
    assert!(top.contains(&Role::Binder {
        namespace: Namespace::Value,
        visibility: Visibility::Hoisted,
        locator: Locator::Field("name"),
    }));
    let nested = roles_of("{ function f() {} }", "function_declaration", None);
    assert!(nested.contains(&Role::Binder {
        namespace: Namespace::Value,
        visibility: Visibility::Whole,
        locator: Locator::Field("name"),
    }));
    // Arrow single param binds (T12); formal params bind as a group.
    // Navigate to the `parameter:` child itself: the body `x` shares its text.
    let src_arrow = "const f = x => x;";
    let t_arrow = parse(src_arrow);
    let arrow = find(t_arrow.root_node(), "arrow_function").pop().unwrap();
    let param = arrow.child_by_field_name("parameter").unwrap();
    assert_eq!(
        JsLang.roles(param, Some("parameter"), src_arrow.as_bytes(), &Env::default()),
        vec![Role::Binder {
            namespace: Namespace::Value,
            visibility: Visibility::Whole,
            locator: Locator::Itself,
        }]
    );
    assert!(roles_of("function f(a, [b]) {}", "formal_parameters", None).contains(
        &Role::Binder {
            namespace: Namespace::Value,
            visibility: Visibility::Whole,
            locator: Locator::Itself,
        }
    ));
}

#[test]
fn js_roles_assignment_targets_are_not_binders() {
    // Identical pattern node kinds, but under `=` they are references (T2).
    assert_eq!(roles_of("({a, b} = src);", "object_pattern", Some("left")), vec![]);
    assert_eq!(roles_of("[e, ...f] = arr;", "array_pattern", Some("left")), vec![]);
    // Kind-less for-in head assigns; kind-ful head binds (T3).
    let no_kind = roles_of("for (h of list) {}", "for_in_statement", None);
    assert!(no_kind.iter().all(|r| !matches!(r, Role::Binder { .. })));
    let with_kind = roles_of("for (let h of list) {}", "for_in_statement", None);
    assert!(with_kind.contains(&Role::Binder {
        namespace: Namespace::Value,
        visibility: Visibility::Whole,
        locator: Locator::Field("left"),
    }));
}

#[test]
fn js_roles_import_export_mirror() {
    let src = r#"import def, {x as y, z} from "m"; export {a as b};"#;
    let tree = parse(src);
    let s = src.as_bytes();
    let env = Env::default();
    // Default import binds.
    let def = find_text(tree.root_node(), "identifier", "def", s);
    assert!(matches!(
        JsLang.roles(def, None, s, &env)[..],
        [Role::Binder { .. }]
    ));
    // `alias ?? name`: alias binds, name-with-alias does not (T4).
    let y = find_text(tree.root_node(), "identifier", "y", s);
    assert!(matches!(
        JsLang.roles(y, Some("alias"), s, &env)[..],
        [Role::Binder { .. }]
    ));
    let x = find_text(tree.root_node(), "identifier", "x", s);
    assert_eq!(JsLang.roles(x, Some("name"), s, &env), vec![]);
    // Bare `name:` (no alias) binds.
    let z = find_text(tree.root_node(), "identifier", "z", s);
    assert!(matches!(
        JsLang.roles(z, Some("name"), s, &env)[..],
        [Role::Binder { .. }]
    ));
    // Export `name:` is a reference; `alias:` is nothing (T4).
    let a = find_text(tree.root_node(), "identifier", "a", s);
    assert_eq!(
        JsLang.roles(a, Some("name"), s, &env),
        vec![Role::Reference {
            namespace: Namespace::Value
        }]
    );
    let b = find_text(tree.root_node(), "identifier", "b", s);
    assert_eq!(JsLang.roles(b, Some("alias"), s, &env), vec![]);
    // Re-export from another module: `name:` is not local.
    let src2 = r#"export {q} from "o";"#;
    let t2 = parse(src2);
    let q = find_text(t2.root_node(), "identifier", "q", src2.as_bytes());
    assert_eq!(
        JsLang.roles(q, Some("name"), src2.as_bytes(), &env),
        vec![]
    );
}

#[test]
fn js_roles_names_and_labels() {
    // `obj.a`: property is free; `o` is a reference.
    assert_eq!(
        roles_of("o.a;", "property_identifier", None),
        vec![]
    );
    assert_eq!(
        roles_of("o.a;", "identifier", None),
        vec![Role::Reference {
            namespace: Namespace::Value
        }]
    );
    // Object-literal shorthand is a reference (T6).
    assert_eq!(
        roles_of("const el = {x};", "shorthand_property_identifier", None),
        vec![Role::Reference {
            namespace: Namespace::Value
        }]
    );
    // Labels: binder at the statement, reference at break.
    let lbl = roles_of("lab: for (;;) { break lab; }", "statement_identifier", Some("label"));
    assert!(lbl.contains(&Role::Binder {
        namespace: Namespace::Label,
        visibility: Visibility::Whole,
        locator: Locator::Itself,
    }));
}

#[test]
fn js_roles_scopes() {
    // A function-body block is not a scope (T9); a bare block is.
    assert_eq!(roles_of("function f() {}", "statement_block", None), vec![]);
    assert_eq!(roles_of("{ let x = 1; }", "statement_block", None).len(), 1);
    // Catch without parameter: no catch scope.
    assert_eq!(roles_of("try {} catch {}", "catch_clause", None), vec![]);
    assert_eq!(roles_of("try {} catch (e) {}", "catch_clause", None).len(), 2);
}

fn round_trip(src: &str, file: &str) -> String {
    let store = svc_core::store::MemStore::new();
    let path = RelPath::new(file).unwrap();
    let snap = ingest_file(src.as_bytes(), path, &JsLang, &store, ChangeId::new()).unwrap();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let rendered = render(&snap, &store, &langs, false).unwrap();
    let bytes = rendered.files.values().next().cloned().unwrap_or_default();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn js_round_trip_twin() {
    assert_eq!(round_trip(TWIN, "src/config.js"), TWIN);
}

#[test]
fn js_round_trip_trivia_and_forms() {
    let src = "// lead\nconst t = `plain ${1 + 2}`;\n/* mid */\nlet d = 2; // trail\n\nfunction* gen(a = 1, ...rest) {\n  yield a;\n}\n\nclass D extends B {\n  #p = 1;\n  m() {\n    return this.#p;\n  }\n  static {\n    let z = 1;\n  }\n}\n\ntry {\n  gen();\n} catch ({ code }) {\n  log(code);\n}\n\nfor (let i = 0; i < 1; i++) {\n  log(i);\n}\n\nconst el = { x: 1 };\n";
    assert_eq!(round_trip(src, "src/extra.js"), src);
}

#[test]
fn js_for_path() {
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let js = RelPath::new("src/config.js").unwrap();
    assert_eq!(langs.for_path(&js).map(|l| l.name()), Some("javascript"));
    let rs = RelPath::new("src/main.rs").unwrap();
    assert!(langs.for_path(&rs).is_none());
}

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../demo/config-js/src")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// Codex's shared JS twin (`rtomsmmm`): accessor pair, static, private
/// field, array-destructuring declarator, export-wrapped items.
#[test]
fn js_round_trip_codex_fixture() {
    for name in ["config.js", "main.js"] {
        let src = fixture(name);
        assert_eq!(round_trip(&src, &format!("src/{name}")), src, "{name}");
    }
    let src = fixture("config.js");
    let tree = parse(&src);
    let s = src.as_bytes();
    let members: Vec<(String, Kind)> = find(tree.root_node(), "method_definition")
        .into_iter()
        .map(|m| {
            let n = m.child_by_field_name("name").unwrap();
            (String::from_utf8_lossy(&s[n.start_byte()..n.end_byte()]).into_owned(), js_kind(m, s).unwrap())
        })
        .collect();
    let kinds = |name: &str| -> Vec<Kind> {
        let mut k: Vec<Kind> = members
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, k)| *k)
            .collect();
        k.sort();
        k
    };
    // Same-name getter/setter stay distinct: no spurious AddAdd (C1/§25).
    assert_eq!(kinds("path"), [Kind::JsGetter, Kind::JsSetter]);
    assert!(members.contains(&("defaults".to_owned(), Kind::JsStaticMethod)));
    assert!(members.contains(&("constructor".to_owned(), Kind::JsMethod)));
    // Private field is an entity named `#path`, distinct from the accessors.
    let fields: Vec<String> = find(tree.root_node(), "field_definition")
        .into_iter()
        .map(|f| {
            let p = f.child_by_field_name("property").unwrap();
            String::from_utf8_lossy(&s[p.start_byte()..p.end_byte()]).into_owned()
        })
        .collect();
    assert!(fields.contains(&"#path".to_owned()));
    // Export-wrapped declarations surface as root entities.
    let raw = extract(&tree, s, &JsLang).unwrap();
    let roots: Vec<&str> = raw
        .iter()
        .filter(|e| e.parent_idx.is_none())
        .map(|e| e.name.as_str())
        .collect();
    for want in ["Config", "read", "parse", "validate", "normalize", "log", "canon", "load"] {
        assert!(roots.contains(&want), "missing root {want}");
    }
}

#[test]
fn js_roles_named_function_and_class_expressions() {
    // Self-binding visible only inside; the outer scope sees nothing new.
    let src = "foo(function bar() {});";
    let tree = parse(src);
    let fe = find(tree.root_node(), "function_expression").pop().unwrap();
    assert!(JsLang
        .roles(fe, None, src.as_bytes(), &Env::default())
        .contains(&Role::Binder {
            namespace: Namespace::Value,
            visibility: Visibility::Whole,
            locator: Locator::Field("name"),
        }));
    let src2 = "const D = class Inner {};";
    let tree2 = parse(src2);
    let class = find(tree2.root_node(), "class").pop().unwrap();
    assert!(JsLang
        .roles(class, None, src2.as_bytes(), &Env::default())
        .contains(&Role::Binder {
            namespace: Namespace::Value,
            visibility: Visibility::Whole,
            locator: Locator::Field("name"),
        }));
}

/// α-invariance through the shared O2 resolver (durable proof the JS
/// Binder roles resolve). Destructured-pattern params are excluded: they
/// need the pending `is_ident_leaf` extension (see inbox/to-cursor).
#[test]
fn js_alpha_invariant() {
    use svc_core::engine::{canonicalize, parse as eng_parse, resolve};
    use svc_core::ids::ContentId;
    fn content_of(src: &str) -> ContentId {
        let lang = JsLang;
        let tree = eng_parse(src.as_bytes(), &lang).unwrap();
        let item = tree.root_node().child(0).expect("one item");
        let res = resolve(item, src.as_bytes(), &lang, &Env::default()).unwrap();
        let c = canonicalize(item, src.as_bytes(), &res, &[], &Env::default(), &lang).unwrap();
        c.id()
    }
    for (a, b, label) in [
        ("function parse(s) { return s; }\n", "function parse(input) { return input; }\n", "param"),
        ("function f(a) { let b = a; return b; }\n", "function f(x) { let y = x; return y; }\n", "locals"),
        ("const f = q => q + 1;\n", "const f = w => w + 1;\n", "arrow-param"),
    ] {
        assert_eq!(content_of(a), content_of(b), "α-rename must preserve content ({label})");
    }
}
