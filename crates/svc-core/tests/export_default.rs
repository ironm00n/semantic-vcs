//! Anonymous `export default` expressions are file-level entities named
//! `default`, not skipped and not `«kind:offset»`. A class body's methods nest
//! under that class instead of becoming file roots.
use std::collections::BTreeMap;

use svc_core::engine::{extract, lookup_name, rename, render, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::lang::Langs;
use svc_core::store::MemStore;
use svc_core::{JsLang, Kind};

fn parse(src: &str) -> tree_sitter::Tree {
    let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    parser.parse(src, None).unwrap()
}

fn roots(src: &str) -> Vec<(String, Kind)> {
    let tree = parse(src);
    let raw = extract(&tree, src.as_bytes(), &JsLang).unwrap();
    raw.iter()
        .filter(|e| e.parent_idx.is_none())
        .map(|e| (e.name.clone(), e.kind))
        .collect()
}

#[test]
fn named_default_export_keeps_its_declared_name() {
    let src = "export default function foo() { return 1; }\n";
    assert_eq!(roots(src), vec![("foo".into(), Kind::JsFunction)]);
}

#[test]
fn anonymous_default_function_is_named_default() {
    let src = "export default function () { return 1; }\n";
    assert_eq!(roots(src), vec![("default".into(), Kind::JsFunction)]);
    let tree = parse(src);
    let raw = extract(&tree, src.as_bytes(), &JsLang).unwrap();
    assert!(
        raw.iter().all(|e| !e.name.contains('«')),
        "no offset-fallback name: {:?}",
        raw.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

#[test]
fn anonymous_default_arrow_is_named_default() {
    let src = "export default () => 1;\n";
    assert_eq!(roots(src), vec![("default".into(), Kind::JsFunction)]);
}

#[test]
fn anonymous_default_class_owns_its_methods() {
    let src = "export default class { m() {} }\n";
    let tree = parse(src);
    let raw = extract(&tree, src.as_bytes(), &JsLang).unwrap();
    let class = raw
        .iter()
        .find(|e| e.kind == Kind::JsClass && e.name == "default")
        .unwrap_or_else(|| panic!("expected a default class, got {raw:?}"));
    let class_idx = raw.iter().position(|e| std::ptr::eq(e, class)).unwrap();
    assert!(class.parent_idx.is_none(), "{raw:?}");
    let members: Vec<&str> = raw
        .iter()
        .filter(|e| e.parent_idx == Some(class_idx))
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(members, ["m"], "{raw:?}");
    assert!(
        raw.iter()
            .all(|e| !(e.kind == Kind::JsMethod && e.parent_idx.is_none())),
        "method must not be a file root: {raw:?}"
    );
}

#[test]
fn a_default_literal_is_still_not_an_entity() {
    assert!(roots("export default 1;\n").is_empty());
}

#[test]
fn anonymous_default_id_is_stable_under_edits_above_it() {
    let src = "export default () => 1;\n";
    let store = MemStore::new();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let path = RelPath::new("src/mod.js").unwrap();
    let snap_of = |src: &[u8], prev: Option<&svc_core::Snapshot>| {
        let mut files = BTreeMap::new();
        files.insert(path.clone(), src.to_vec());
        snapshot_files(&store, &langs, &files, prev, ChangeId::new()).unwrap()
    };
    let before = snap_of(src.as_bytes(), None);
    let (id, rec) = before
        .entities
        .iter()
        .find(|(_, r)| r.name == "default" && r.kind == Kind::JsFunction)
        .expect("anonymous default export is an entity");
    assert!(!rec.name.contains('«'), "{}", rec.name);
    let after_src = "// a comment line\n".to_owned() + src;
    let after = snap_of(after_src.as_bytes(), Some(&before));
    let rec_after = after
        .entities
        .get(id)
        .expect("the default export kept its id");
    assert_eq!(rec_after.name, "default");
    assert_eq!(rec_after.kind, Kind::JsFunction);
    let rendered = render(&after, &store, &langs, false).unwrap();
    assert_eq!(
        rendered.files.get(&path).map(|b| b.as_slice()),
        Some(after_src.as_bytes())
    );
}

#[test]
fn rename_refuses_an_anonymous_default_export() {
    let src = "export default function () { return 1; }\n";
    let store = MemStore::new();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let path = RelPath::new("src/mod.js").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "default" && r.kind == Kind::JsFunction)
        .map(|(id, _)| *id)
        .expect("anonymous default");
    let err = rename(&store, &snap, id, "dflt2").unwrap_err().to_string();
    assert!(err.contains("no name spelling"), "{err}");
    assert_eq!(snap.entities[&id].name, "default");
}

#[test]
fn rename_rewrites_a_named_default_export() {
    let src = "export default function foo() { return 1; }\n";
    let store = MemStore::new();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let path = RelPath::new("src/mod.js").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = lookup_name(&snap, "foo").unwrap();
    let next = rename(&store, &snap, id, "bar").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("function bar()"), "{text}");
    assert!(!text.contains("function foo()"), "{text}");
}
