//! Kindless file-level statements are not entities. They still belong to a
//! neighbouring file-root so `rename` rewrites uses there.
use std::collections::BTreeMap;

use svc_core::engine::{lookup_name, rename, render, snapshot_files};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::{JsLang, Langs};

fn js_snap(src: &str) -> (MemStore, Langs, RelPath, svc_core::Snapshot) {
    let store = MemStore::new();
    let langs = Langs::new(vec![Box::new(JsLang)]);
    let path = RelPath::new("src/main.js").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    (store, langs, path, snap)
}

fn rendered(store: &MemStore, langs: &Langs, snap: &svc_core::Snapshot, path: &RelPath) -> String {
    String::from_utf8(render(snap, store, langs, false).unwrap().files[path].clone()).unwrap()
}

#[test]
fn rename_rewrites_kindless_for_of_in_the_file_tail() {
    let src = "let h = 0;\nfor (h of list) {\n  log(h);\n}\n";
    let (store, langs, path, snap) = js_snap(src);
    assert_eq!(
        snap.entities.values().filter(|e| e.name == "h").count(),
        1,
        "the loop is tail, not a second entity"
    );
    let id = lookup_name(&snap, "h").unwrap();
    let next = rename(&store, &snap, id, "q").unwrap();
    let text = rendered(&store, &langs, &next, &path);
    assert_eq!(text, "let q = 0;\nfor (q of list) {\n  log(q);\n}\n");
}

#[test]
fn rename_rewrites_kindless_for_between_two_declarators() {
    let src = "let h = 0;\nfor (h of list) {\n  log(h);\n}\nlet z = 1;\n";
    let (store, langs, path, snap) = js_snap(src);
    let id = lookup_name(&snap, "h").unwrap();
    let next = rename(&store, &snap, id, "q").unwrap();
    let text = rendered(&store, &langs, &next, &path);
    assert_eq!(
        text,
        "let q = 0;\nfor (q of list) {\n  log(q);\n}\nlet z = 1;\n"
    );
}

#[test]
fn rename_does_not_rewrite_c_style_for_let_shadow() {
    let src = "let i = 99;\nfor (let i = 0; i < 1; i++) {\n  log(i);\n}\nlog(i);\n";
    let (store, langs, path, snap) = js_snap(src);
    let id = lookup_name(&snap, "i").unwrap();
    let next = rename(&store, &snap, id, "q").unwrap();
    let text = rendered(&store, &langs, &next, &path);
    assert_eq!(
        text,
        "let q = 99;\nfor (let i = 0; i < 1; i++) {\n  log(i);\n}\nlog(q);\n"
    );
}

#[test]
fn file_tail_round_trips() {
    let src = "let h = 0;\nfor (h of list) {\n  log(h);\n}\n";
    let (store, langs, path, snap) = js_snap(src);
    assert_eq!(rendered(&store, &langs, &snap, &path), src);
}
