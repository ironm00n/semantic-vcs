use std::collections::BTreeMap;

use svc_core::engine::{
    ingest_file, lookup_name, redefine, rename, render, rust_langs, snapshot_files, status_report,
};
use svc_core::ids::{ChangeId, RelPath};
use svc_core::store::MemStore;
use svc_core::{IdentRef, RustLang, Store, Token};

const SRC: &str = r#"fn parse(s: &str) -> usize { s.len() }
fn load(path: &str) -> usize { parse(path) }
"#;

#[test]
fn o3_rename_does_not_touch_other_hashes() {
    let store = MemStore::new();
    let lang = RustLang;
    let path = RelPath::new("src/lib.rs").unwrap();
    let snap = ingest_file(SRC.as_bytes(), path, &lang, &store, ChangeId::new()).unwrap();
    let parse_id = lookup_name(&snap, "parse").unwrap();
    let load_id = lookup_name(&snap, "load").unwrap();
    let load_before = snap.entities[&load_id].clone();
    let parse_before = snap.entities[&parse_id].clone();

    let next = rename(&snap, parse_id, "parse_config").unwrap();
    let load_after = &next.entities[&load_id];
    let parse_after = &next.entities[&parse_id];

    assert_eq!(parse_after.name, "parse_config");
    assert_eq!(parse_after.content, parse_before.content);
    assert_eq!(parse_after.bytes, parse_before.bytes);
    assert_eq!(load_after.content, load_before.content);
    assert_eq!(load_after.bytes, load_before.bytes);

    let langs = rust_langs();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files.values().next().unwrap().clone()).unwrap();
    assert!(text.contains("fn parse_config"), "{text}");
    assert!(text.contains("parse_config(path)"), "{text}");
    assert!(!text.contains("fn parse("), "{text}");
}

#[test]
fn snapshot_files_matches_ingest_and_status_is_clean() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), SRC.as_bytes().to_vec());
    let change = ChangeId::new();
    let snap = snapshot_files(&store, &langs, &files, None, change).unwrap();
    assert!(snap.entities.values().any(|e| e.name == "parse"));
    assert!(snap.entities.values().any(|e| e.name == "load"));
    let again = snapshot_files(&store, &langs, &files, Some(&snap), change).unwrap();
    let report = status_report(&snap, &again);
    assert_eq!(report.summary(), format!("{} entities, 0 changes", snap.entities.len()));
}

#[test]
fn redefine_keeps_callees_as_entity_holes() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let load_id = lookup_name(&snap, "load").unwrap();
    let parse_id = lookup_name(&snap, "parse").unwrap();
    let text = b"fn load(path: &str) -> usize { parse(path) + 1 }\n";
    let (content, bytes) = redefine(&store, &langs, &snap, load_id, text).unwrap();
    let c = store.get_content(content).unwrap();
    assert!(
        c.tokens
            .iter()
            .any(|t| match t {
                Token::Ident(IdentRef::Entity(id)) => *id == parse_id,
                _ => false,
            }),
        "callee must resolve to parse's entity id, got {:?}",
        c.tokens
    );
    let _ = bytes;
}

#[test]
fn layout_local_rename_is_not_semantic() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path.clone(), SRC.as_bytes().to_vec());
    let change = ChangeId::new();
    let snap = snapshot_files(&store, &langs, &files, None, change).unwrap();
    let edited = SRC.replace("fn parse(s: &str) -> usize { s.len() }", "fn parse(input: &str) -> usize { input.len() }");
    files.insert(path, edited.into_bytes());
    let next = snapshot_files(&store, &langs, &files, Some(&snap), change).unwrap();
    let report = status_report(&snap, &next);
    assert_eq!(report.semantic, 0, "{:?}", report.deltas);
    assert_eq!(report.layout, 1, "{:?}", report.deltas);
}

#[test]
fn redefine_without_trailing_newline_keeps_callee_entity() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let load_id = lookup_name(&snap, "load").unwrap();
    let parse_id = lookup_name(&snap, "parse").unwrap();
    let text = b"fn load(path: &str) -> usize { parse(path) + 1 }";
    assert_ne!(text.last(), Some(&b'\n'));
    let (content, _) = redefine(&store, &langs, &snap, load_id, text).unwrap();
    let c = store.get_content(content).unwrap();
    assert!(
        c.tokens.iter().any(|t| match t {
            Token::Ident(IdentRef::Entity(id)) => *id == parse_id,
            _ => false,
        }),
        "callee must resolve to parse's entity id, got {:?}",
        c.tokens
    );
}
