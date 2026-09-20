//! JS class members can be added and edited by typed ops. A bare `m() {}` is not a
//! program, so the engine wraps it in the language's member shell to parse it.
use std::collections::BTreeMap;

use svc_core::engine::{add_def, edit_def, lookup_name, render, snapshot_files};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::store::MemStore;
use svc_core::{Intent, JsLang, Kind, Langs, ObservedClass};

const SRC: &str = "export class Config {\n  #path\n  constructor(path) {\n    this.#path = path\n  }\n  get path() {\n    return this.#path\n  }\n  toString() {\n    return `Config(${this.#path})`\n  }\n}\n\nexport function make(p) {\n  return new Config(p)\n}\n";

fn setup() -> (MemStore, Langs, RelPath, svc_core::Snapshot) {
    let store = MemStore::new();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let path = RelPath::new("src/config.js").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    (store, langs, path, snap)
}

fn text(store: &MemStore, langs: &Langs, path: &RelPath, snap: &svc_core::Snapshot) -> String {
    String::from_utf8(render(snap, store, langs, false).unwrap().files[path].clone()).unwrap()
}

#[test]
fn add_def_of_a_method_into_a_class() {
    let (store, langs, path, snap) = setup();
    let class = lookup_name(&snap, "Config").unwrap();
    let next = add_def(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        Some(class),
        4,
        b"isValid() {\n  return this.#path.length > 0\n}",
        Intent::Feature,
    )
    .unwrap();
    let out = text(&store, &langs, &path, &next);
    assert!(
        out.contains("\n\n  isValid() {\n    return this.#path.length > 0\n  }\n}\n"),
        "no blank line before the closing brace:\n{out}"
    );
    assert!(!out.contains("__svc_shell__"), "{out}");
    let m = next
        .entities
        .values()
        .find(|r| r.name == "isValid")
        .unwrap();
    assert_eq!((m.kind, m.parent), (Kind::JsMethod, Some(class)));
    let re = snapshot_files(
        &store,
        &langs,
        &BTreeMap::from([(path, out.into_bytes())]),
        Some(&next),
        next.change,
    )
    .unwrap();
    assert_eq!(re.entities.len(), next.entities.len());
}

#[test]
fn edit_def_of_a_method_and_a_getter() {
    let (store, langs, path, snap) = setup();
    let class = lookup_name(&snap, "Config").unwrap();
    let to_string = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "toString")
        .map(|(id, _)| *id)
        .unwrap();
    let (next, class_of_edit) = edit_def(
        &store,
        &langs,
        &snap,
        to_string,
        b"toString() {\n    return `Config<${this.#path}>`\n  }",
    )
    .unwrap();
    assert_eq!(class_of_edit, ObservedClass::BindingPreserving);
    let out = text(&store, &langs, &path, &next);
    assert!(
        out.contains("  toString() {\n    return `Config<${this.#path}>`\n  }\n}"),
        "{out}"
    );
    assert!(!out.contains("__svc_shell__"), "{out}");

    let getter = snap
        .entities
        .iter()
        .find(|(_, r)| r.kind == Kind::JsGetter)
        .map(|(id, _)| *id)
        .unwrap();
    let (next2, _) = edit_def(
        &store,
        &langs,
        &next,
        getter,
        b"get path() {\n    return String(this.#path)\n  }",
    )
    .unwrap();
    assert_eq!(next2.entities[&getter].kind, Kind::JsGetter);
    assert_eq!(next2.entities[&getter].parent, Some(class));
    let out2 = text(&store, &langs, &path, &next2);
    assert!(
        out2.contains("  get path() {\n    return String(this.#path)\n  }\n"),
        "{out2}"
    );

    let err = edit_def(
        &store,
        &langs,
        &next2,
        getter,
        b"set path(v) { this.#path = v }",
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("cannot change"),
        "getter → setter is a kind change: {err}"
    );
}

#[test]
fn a_bare_method_that_does_not_parse_is_still_refused() {
    let (store, langs, _, snap) = setup();
    let class = lookup_name(&snap, "Config").unwrap();
    let err = add_def(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        Some(class),
        4,
        b"isValid( { }",
        Intent::Feature,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not parse"), "{err}");
}
