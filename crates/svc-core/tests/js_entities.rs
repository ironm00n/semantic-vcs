//! lang-js lane: entity extraction, Kind refinement, roles, round-trip.
//! Binding data: `facts/js-binding-tables.md`; Kind variants: DECISIONS §25.

use std::collections::BTreeMap;

use svc_core::engine::{
    add_def, edit_def, extract, ingest_file, lookup_name, rename, render, snapshot_files,
};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::lang::{Env, Lang, Langs, Locator, Role, Visibility};
use svc_core::store::MemStore;
use svc_core::Namespace;
use svc_core::{js_kind, IdentRef, Intent, JsLang, Kind, ObservedClass};

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
        ("function f({a, b}) { return a; }\n", "function f({x, y}) { return x; }\n", "destructured"),
    ] {
        assert_eq!(content_of(a), content_of(b), "α-rename must preserve content ({label})");
    }
}

/// Refined kinds at the `extract` level (§8/C1/§25): the override, not just
/// `js_kind`, decides entity identity for accessors, statics, and
/// function-valued declarators.
#[test]
fn js_extract_refined_kinds() {
    let tree = parse(TWIN);
    let raw = extract(&tree, TWIN.as_bytes(), &JsLang).unwrap();
    let class_idx = raw.iter().position(|e| e.name == "Config").unwrap();
    let mut members: Vec<(&str, Kind)> = raw
        .iter()
        .filter(|e| e.parent_idx == Some(class_idx))
        .map(|e| (e.name.as_str(), e.kind))
        .collect();
    members.sort();
    assert_eq!(
        members,
        [
            ("load", Kind::JsStaticMethod),
            ("path", Kind::JsGetter),
            ("path", Kind::JsSetter),
        ]
    );
    // Module-scope declarators only; function-valued ones are functions.
    let src = "const f = x => x;\nconst n = 1;\nfunction g() {\n  const inner = 2;\n  return inner;\n}\n";
    let tree2 = parse(src);
    let raw2 = extract(&tree2, src.as_bytes(), &JsLang).unwrap();
    let mut roots: Vec<(&str, Kind)> = raw2
        .iter()
        .filter(|e| e.parent_idx.is_none())
        .map(|e| (e.name.as_str(), e.kind))
        .collect();
    roots.sort();
    assert_eq!(
        roots,
        [
            ("f", Kind::JsFunction),
            ("g", Kind::JsFunction),
            ("n", Kind::JsDeclarator),
        ]
    );
    // The nested declarator is a local, not a child entity (§12).
    assert!(raw2.iter().all(|e| e.name != "inner"));
}

/// JS line-7 analog (mirrors `o8_shadowing_let_is_binding_changing`): a
/// binder shadows a parameter, so the edit must classify as
/// `BindingChanging`. Answers: does JS get the same classifier as Rust?
/// (Same-scope shape as the Rust test: the inserted line keeps the use line
/// textually intact so the line-aligning classifier can see the retarget.
/// The static model does not enforce the redeclaration early-error.)
#[test]
fn js_edit_def_shadow_param_is_binding_changing() {
    let store = MemStore::new();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let path = RelPath::new("src/config.js").unwrap();
    let src = "function canon(c) {\n  return { retries: c.retries };\n}\nfunction validate(c) {\n  return c.retries > 10;\n}\n";
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "validate").unwrap();
    let new = b"function validate(c) {\n  const c = canon(c);\n  return c.retries > 10;\n}\n";
    let (_, class) = edit_def(&store, &langs, &snap, id, new).unwrap();
    assert_eq!(class, ObservedClass::BindingChanging);
}

/// Scripted JS agent stand-in (no model): the JS analog of the line-9 Rust
/// agent run — rename `read`→`read_file`, add_def `checkRetries`, edit_def
/// `validate` to call it — performed with engine ops on the real
/// `demo/config-js` sources and recorded as `demo/recordings/js-agent.jsonl`
/// in the `line9.ops.jsonl` shape.
#[test]
fn js_scripted_agent_stand_in() {
    let store = MemStore::new();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let config_path = RelPath::new("src/config.js").unwrap();
    let main_path = RelPath::new("src/main.js").unwrap();
    let mut files = BTreeMap::new();
    files.insert(config_path, fixture("config.js").into_bytes());
    files.insert(main_path, fixture("main.js").into_bytes());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();

    // Op 1: rename `read` → `read_file`.
    let read_id = lookup_name(&snap, "read").unwrap();
    let snap = rename(&snap, read_id, "read_file").unwrap();
    assert!(lookup_name(&snap, "read").is_err());
    let read_file_id = lookup_name(&snap, "read_file").unwrap();
    assert_eq!(read_file_id, read_id);

    // Op 2: add_def `checkRetries` as a new root after the existing roots.
    let ordinal = snap
        .entities
        .values()
        .filter(|r| r.parent.is_none())
        .count() as u32;
    let check_body = "export function checkRetries(config) {\n  if (config.retries > 10) throw new Error('retries must not exceed 10')\n}\n";
    let snap = add_def(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        None,
        ordinal,
        check_body.as_bytes(),
        Intent::Refactor,
    )
    .unwrap();
    let check_id = lookup_name(&snap, "checkRetries").unwrap();

    // Op 3: edit_def `validate` to call it; the surviving `if` line keeps
    // its targets, so this must stay BindingPreserving like the Rust run.
    let validate_id = lookup_name(&snap, "validate").unwrap();
    let new_validate = "export function validate(config) {\n  checkRetries(config)\n  if (config.retries > 10) throw new Error('retries must not exceed 10')\n}\n";
    let (snap, class) = edit_def(&store, &langs, &snap, validate_id, new_validate.as_bytes())
        .unwrap();
    assert_eq!(class, ObservedClass::BindingPreserving);

    // Rename propagated to the caller: rendered `load` calls `read_file`.
    let rendered = render(&snap, &store, &langs, false).unwrap();
    let text: String = rendered
        .files
        .values()
        .flat_map(|b| String::from_utf8(b.clone()))
        .collect();
    assert!(
        text.contains("read_file(path)"),
        "rename did not propagate to load:\n{text}"
    );
    assert_ne!(check_id, validate_id);

    // Record the three ops in the line9.ops.jsonl shape.
    let lines = [
        serde_json::json!({"op": "rename", "entity": "read", "new_name": "read_file"}),
        serde_json::json!({"op": "add_def", "ordinal": ordinal, "intent": "refactor", "definition": check_body}),
        serde_json::json!({"op": "edit_def", "entity": "validate", "intent": "refactor",
            "note": "call checkRetries(config) before the retries check; observed BindingPreserving",
            "patch": {"find": "  if (config.retries > 10)",
                       "replace": "  checkRetries(config)\n  if (config.retries > 10)"}}),
    ];
    // The recording is a pinned demo artifact: compare against it read-only.
    // (An earlier revision rewrote the file with `serde_json::to_string`,
    // whose sorted keys dirtied the tree on every test run.)
    let dest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../demo/recordings/js-agent.jsonl");
    let recorded = std::fs::read_to_string(&dest).unwrap_or_else(|e| panic!("{dest:?}: {e}"));
    let recorded: Vec<serde_json::Value> = recorded
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let expected: Vec<serde_json::Value> = lines.iter().cloned().collect();
    assert_eq!(recorded, expected, "js-agent.jsonl drifted from the scripted run");
}

/// Trap T5: `default` in a specifier is an anonymous token — the visible
/// `alias:` still binds, and an export `alias:` still binds nothing.
#[test]
fn js_trap_specifier_default_tokens() {
    let src = "import {default as d} from \"m\";";
    let tree = parse(src);
    let d = find_text(tree.root_node(), "identifier", "d", src.as_bytes());
    assert!(
        matches!(
            JsLang.roles(d, Some("alias"), src.as_bytes(), &Env::default())[..],
            [Role::Binder { .. }]
        ),
        "default-import alias must bind"
    );
    let src2 = "export {default as e};";
    let tree2 = parse(src2);
    let e = find_text(tree2.root_node(), "identifier", "e", src2.as_bytes());
    assert_eq!(
        JsLang.roles(e, Some("alias"), src2.as_bytes(), &Env::default()),
        vec![]
    );
}

/// Free-reference names across every top-level item of `src`.
fn free_ref_names(src: &str) -> Vec<String> {
    use svc_core::engine::{parse as eng_parse, resolve};
    let lang = JsLang;
    let tree = eng_parse(src.as_bytes(), &lang).unwrap();
    let mut names = vec![];
    let mut cursor = tree.walk();
    for item in tree.root_node().children(&mut cursor) {
        if !item.is_named() {
            continue;
        }
        let res = resolve(item, src.as_bytes(), &lang, &Env::default()).unwrap();
        for (_, ident) in &res.refs {
            if let IdentRef::Free(n) = ident {
                names.push(n.to_string());
            }
        }
    }
    names.sort();
    names
}

/// Traps T13/T14/T15/T18/T21: `meta_property` emits no refs; tagged-template
/// tags/subs, `extends` heads, parenthesized targets, and optional chains
/// keep theirs.
#[test]
fn js_trap_expression_refs_survive() {
    // T15: `class_heritage` is an unnamed child — the extends head must not
    // be lost by a fields-only walk.
    assert!(free_ref_names("class C extends Base {}").contains(&"Base".to_string()));
    // T14: `arguments:` is a `template_string`, not an `arguments` node —
    // both the tag and the substitution resolve.
    let tagged = free_ref_names("tag`hi ${x}`;");
    assert!(tagged.contains(&"tag".to_string()), "{tagged:?}");
    assert!(tagged.contains(&"x".to_string()), "{tagged:?}");
    // T18: parenthesized targets unwrap before the T2/T3 pattern rules.
    let paren = free_ref_names("for ((h) of list) { log(h); }");
    assert_eq!(paren.iter().filter(|n| *n == "h").count(), 2, "{paren:?}");
    assert!(paren.contains(&"list".to_string()), "{paren:?}");
    // T21: `optional_chain` is a named node — skip it, keep the object.
    assert!(free_ref_names("o?.p;").contains(&"o".to_string()));
    // T13: one node kind for `new.target`/`import.meta`, no refs either way.
    assert_eq!(free_ref_names("new.target;"), Vec::<String>::new());
    assert_eq!(free_ref_names("import.meta;"), Vec::<String>::new());
    // T7: reserved words alias to `identifier` — still ordinary references.
    let reserved = free_ref_names("get(of);");
    assert!(reserved.contains(&"get".to_string()), "{reserved:?}");
    assert!(reserved.contains(&"of".to_string()), "{reserved:?}");
}

/// Trap T11: `switch_statement value:` is evaluated before the `switch_body`
/// scope exists, so the discriminant never sees a case-level `let`.
#[test]
fn js_trap_switch_discriminant_outside_case_scope() {
    let src = "let d = 0;\nswitch (d) {\n  case 1: {\n    let d = 2;\n  }\n}\n";
    // The only free reference is the discriminant's `d`: neither the outer
    // declarator (a binder, not a ref) nor the case `let` (a local) leaks.
    assert_eq!(free_ref_names(src), ["d".to_string()]);
}

/// Trap T22: array holes produce no child — `[a, , b]` still yields both
/// binders-turned-references in an assignment target.
#[test]
fn js_trap_array_holes_skip_positions() {
    assert_eq!(
        free_ref_names("[a, , b] = arr;"),
        ["a".to_string(), "arr".to_string(), "b".to_string()]
    );
}

/// `using` / `await using` bind exactly like `const` (facts §5, cheap-keep).
#[test]
fn js_roles_using_declarations_bind_lexically() {
    assert!(roles_of("using x = f();", "variable_declarator", None).contains(&Role::Binder {
        namespace: Namespace::Value,
        visibility: Visibility::Whole,
        locator: Locator::Field("name"),
    }));
}

/// Trap T19 vs SPEC §68 (corrected 9/18): computed member names get the
/// source text as the synthetic name — `[k]`, not `None` — and are instead
/// never matched across a rename (engine-side rule, not the lane's).
#[test]
fn js_trap_computed_member_name_keeps_source_text() {
    let src = "class A {\n  [k]() {}\n}\n";
    let tree = parse(src);
    let method = find(tree.root_node(), "method_definition").pop().unwrap();
    assert_eq!(
        JsLang.entity_name(method, src.as_bytes()),
        Some("[k]".to_string())
    );
}
