use std::collections::BTreeMap;

use svc_core::engine::{
    add_def, edit_def, ingest_file, lookup_name, redefine, rename, render, rust_langs,
    snapshot_files, status_report,
};
use svc_core::ids::{ChangeId, EntityId, RelPath};
use svc_core::store::MemStore;
use svc_core::{IdentRef, Intent, RustLang, Store, Token};

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
    let report = status_report(&store, &snap, &again).unwrap();
    assert_eq!(
        report.summary(),
        format!("{} entities, 0 changes", snap.entities.len())
    );
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
    let (_, content, bytes) = redefine(&store, &langs, &snap, load_id, text).unwrap();
    let c = store.get_content(content).unwrap();
    assert!(
        c.tokens.iter().any(|t| match t {
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
    let edited = SRC.replace(
        "fn parse(s: &str) -> usize { s.len() }",
        "fn parse(input: &str) -> usize { input.len() }",
    );
    files.insert(path, edited.into_bytes());
    let next = snapshot_files(&store, &langs, &files, Some(&snap), change).unwrap();
    let report = status_report(&store, &snap, &next).unwrap();
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
    let (_, content, _) = redefine(&store, &langs, &snap, load_id, text).unwrap();
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

#[test]
fn add_def_separates_from_the_previous_item() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let src = "struct Error(String);\nfn load() {}\n";
    let mut files = BTreeMap::new();
    files.insert(path, src.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let next = add_def(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        None,
        1,
        b"fn check_retries() {}",
        Intent::Feature,
    )
    .unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files.values().next().unwrap().clone()).unwrap();
    assert!(
        !text.contains("String);fn"),
        "add-def glued onto the previous item:\n{text}"
    );
    assert!(text.contains("fn check_retries"), "{text}");
}

#[test]
fn add_def_honours_an_explicit_file() {
    use svc_core::engine::add_def_at;
    let store = MemStore::new();
    let langs = rust_langs();
    let a = RelPath::new("src/a.rs").unwrap();
    let b = RelPath::new("src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(a.clone(), b"fn a() {}\n".to_vec());
    files.insert(b.clone(), b"fn b() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let next = add_def_at(
        &store,
        &langs,
        &snap,
        EntityId::new(),
        None,
        Some(b.clone()),
        1,
        b"fn c() {}\n",
        Intent::Feature,
    )
    .unwrap();
    let rec = next.entities.values().find(|e| e.name == "c").expect("c");
    assert_eq!(rec.file, b);
    let rendered = render(&next, &store, &langs, false).unwrap();
    let b_src = String::from_utf8_lossy(rendered.files.get(&b).unwrap());
    assert!(b_src.contains("fn c"), "{b_src}");
    let a_src = String::from_utf8_lossy(rendered.files.get(&a).unwrap());
    assert!(!a_src.contains("fn c"), "{a_src}");
}

fn named_child<'a>(snap: &'a svc_core::Snapshot, name: &str, parent: Option<EntityId>) -> EntityId {
    snap.entities
        .iter()
        .find(|(_, rec)| rec.name == name && rec.parent == parent)
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("missing {name} under {parent:?}"))
}

#[test]
fn rename_rewrites_same_impl_self_and_self_path_calls() {
    const SRC: &str = r#"
struct S;
impl S {
    fn read(&self) {}
    fn load(&self) {
        self.read();
        Self::read();
    }
}
"#;
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let impl_id = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.kind == svc_core::Kind::Impl)
        .map(|(id, _)| *id)
        .expect("impl");
    let method = named_child(&snap, "read", Some(impl_id));
    let load = lookup_name(&snap, "load").unwrap();
    let content = store.get_content(snap.entities[&load].content).unwrap();
    let hits = content
        .tokens
        .iter()
        .filter(|t| matches!(t, Token::Ident(IdentRef::Entity(id)) if *id == method))
        .count();
    assert_eq!(
        hits, 2,
        "self.read and Self::read must be the impl method, got {:?}",
        content.tokens
    );

    let next = rename(&snap, method, "read_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files.values().next().unwrap().clone()).unwrap();
    assert!(text.contains("fn read_file"), "{text}");
    assert!(text.contains("self.read_file()"), "{text}");
    assert!(text.contains("Self::read_file()"), "{text}");
    assert!(!text.contains("self.read()"), "{text}");
    assert!(!text.contains("Self::read()"), "{text}");
}

#[test]
fn rename_rewrites_same_impl_type_path_calls() {
    const SRC: &str = r#"
struct S;
struct Other;
impl S {
    fn make() -> S {
        S
    }
    fn load() {
        let _ = S::make();
        let _ = Other::make();
    }
}
impl Other {
    fn make() -> Other {
        Other
    }
}
"#;
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let impl_s = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.kind == svc_core::Kind::Impl && rec.name == "impl<S>")
        .map(|(id, _)| *id)
        .expect("impl S");
    let method = named_child(&snap, "make", Some(impl_s));
    let load = lookup_name(&snap, "load").unwrap();
    let content = store.get_content(snap.entities[&load].content).unwrap();
    let hits = content
        .tokens
        .iter()
        .filter(|t| matches!(t, Token::Ident(IdentRef::Entity(id)) if *id == method))
        .count();
    assert_eq!(
        hits, 1,
        "S::make must be the impl method; Other::make must not, got {:?}",
        content.tokens
    );

    let next = rename(&snap, method, "create").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files.values().next().unwrap().clone()).unwrap();
    assert!(text.contains("fn create() -> S"), "{text}");
    assert!(text.contains("S::create()"), "{text}");
    assert!(text.contains("Other::make()"), "{text}");
    assert!(text.contains("fn make() -> Other"), "{text}");
    assert!(!text.contains("S::make()"), "{text}");
}

#[test]
fn rename_leaves_typed_receiver_method_calls_untracked() {
    const SRC: &str = r#"
struct S;
impl S {
    fn read(&self) {}
    fn load(&self, other: &S) {
        other.read();
    }
}
"#;
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let impl_id = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.kind == svc_core::Kind::Impl)
        .map(|(id, _)| *id)
        .expect("impl");
    let method = named_child(&snap, "read", Some(impl_id));
    let next = rename(&snap, method, "read_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files.values().next().unwrap().clone()).unwrap();
    assert!(text.contains("fn read_file"), "{text}");
    assert!(
        text.contains("other.read()"),
        "x.method() needs types and must stay untracked:\n{text}"
    );
}

#[test]
fn edit_def_of_a_method_keeps_self_calls_as_entity_holes() {
    const SRC: &str = r#"
struct S;
impl S {
    fn read(&self) {}
    fn load(&self) {
        self.read();
    }
}
"#;
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let impl_id = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.kind == svc_core::Kind::Impl)
        .map(|(id, _)| *id)
        .expect("impl");
    let method = named_child(&snap, "read", Some(impl_id));
    let load = lookup_name(&snap, "load").unwrap();
    let (next, _) = edit_def(
        &store,
        &langs,
        &snap,
        load,
        b"fn load(&self) {\n        self.read();\n        Self::read();\n    }\n",
    )
    .unwrap();
    let content = store.get_content(next.entities[&load].content).unwrap();
    let hits = content
        .tokens
        .iter()
        .filter(|t| matches!(t, Token::Ident(IdentRef::Entity(id)) if *id == method))
        .count();
    assert_eq!(
        hits, 2,
        "edit-def must keep same-impl calls as entity holes, got {:?}",
        content.tokens
    );
}

#[test]
fn rename_rewrites_generic_impl_type_path_calls() {
    const SRC: &str = r#"
struct Wrap<T>(T);
impl<T> Wrap<T> {
    fn make() {}
    fn load() {
        Wrap::make();
    }
}
"#;
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let impl_id = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.kind == svc_core::Kind::Impl)
        .map(|(id, _)| *id)
        .expect("impl");
    let method = named_child(&snap, "make", Some(impl_id));
    let load = lookup_name(&snap, "load").unwrap();
    let content = store.get_content(snap.entities[&load].content).unwrap();
    assert!(
        content
            .tokens
            .iter()
            .any(|t| matches!(t, Token::Ident(IdentRef::Entity(id)) if *id == method)),
        "Wrap::make must be the impl method, got {:?}",
        content.tokens
    );
    let next = rename(&snap, method, "create").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files.values().next().unwrap().clone()).unwrap();
    assert!(text.contains("Wrap::create()"), "{text}");
    assert!(!text.contains("Wrap::make()"), "{text}");
}

#[test]
fn bare_call_inside_impl_is_the_free_fn_not_the_method() {
    const SRC: &str = r#"
fn read() {}
struct S;
impl S {
    fn read(&self) {}
    fn load(&self) {
        read();
        self.read();
    }
}
"#;
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(path, SRC.as_bytes().to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let impl_id = snap
        .entities
        .iter()
        .find(|(_, rec)| rec.kind == svc_core::Kind::Impl)
        .map(|(id, _)| *id)
        .expect("impl");
    let free = named_child(&snap, "read", None);
    let method = named_child(&snap, "read", Some(impl_id));
    let load = lookup_name(&snap, "load").unwrap();
    let content = store.get_content(snap.entities[&load].content).unwrap();
    let entity_hits: Vec<EntityId> = content
        .tokens
        .iter()
        .filter_map(|t| match t {
            Token::Ident(IdentRef::Entity(id)) => Some(*id),
            _ => None,
        })
        .collect();
    assert!(
        entity_hits.contains(&free),
        "bare read() must be the free fn, got {entity_hits:?} tokens {:?}",
        content.tokens
    );
    assert!(
        entity_hits.contains(&method),
        "self.read() must be the method, got {entity_hits:?}"
    );

    let renamed_free = rename(&snap, free, "fetch").unwrap();
    let text = String::from_utf8(
        render(&renamed_free, &store, &langs, false)
            .unwrap()
            .files
            .values()
            .next()
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert!(text.contains("fn fetch()"), "{text}");
    assert!(text.contains("        fetch();"), "{text}");
    assert!(text.contains("        self.read();"), "{text}");
    assert!(text.contains("    fn read(&self)"), "{text}");

    let renamed_method = rename(&snap, method, "pull").unwrap();
    let text = String::from_utf8(
        render(&renamed_method, &store, &langs, false)
            .unwrap()
            .files
            .values()
            .next()
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert!(text.contains("    fn pull(&self)"), "{text}");
    assert!(text.contains("        self.pull();"), "{text}");
    assert!(text.contains("        read();"), "{text}");
    assert!(text.contains("fn read()"), "{text}");
}

fn named_in_file<'a>(snap: &'a svc_core::Snapshot, name: &str, file: &RelPath) -> EntityId {
    snap.entities
        .iter()
        .find(|(_, rec)| rec.name == name && rec.file == *file)
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("{name} in {file}"))
}

#[test]
fn rename_same_named_fn_does_not_rewrite_the_other_crate() {
    let store = MemStore::new();
    let langs = rust_langs();
    let core = RelPath::new("crates/svc-core/src/ids.rs").unwrap();
    let repo = RelPath::new("crates/svc-repo/src/store.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        core.clone(),
        b"fn hex32(b: &[u8; 32]) -> String { format!(\"{b:?}\") }\nfn id_text() -> String { hex32(&[0; 32]) }\n".to_vec(),
    );
    files.insert(
        repo.clone(),
        b"fn hex32(b: &[u8; 32]) -> String { format!(\"{b:x?}\") }\nfn store_text() -> String { hex32(&[1; 32]) }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let repo_id = named_in_file(&snap, "hex32", &repo);
    let next = rename(&snap, repo_id, "hex_of_id").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let core_txt = String::from_utf8(rendered.files[&core].clone()).unwrap();
    let repo_txt = String::from_utf8(rendered.files[&repo].clone()).unwrap();
    assert!(core_txt.contains("fn hex32"), "{core_txt}");
    assert!(core_txt.contains("hex32(&[0; 32])"), "{core_txt}");
    assert!(!core_txt.contains("hex_of_id"), "{core_txt}");
    assert!(repo_txt.contains("fn hex_of_id"), "{repo_txt}");
    assert!(repo_txt.contains("hex_of_id(&[1; 32])"), "{repo_txt}");
}

#[test]
fn rename_does_not_rewrite_a_foreign_use_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("crates/svc-forge/src/lib.rs").unwrap();
    let http = RelPath::new("crates/svc-forge/tests/http.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"use axum::response::{Html, IntoResponse};\nfn serve() {}\n".to_vec(),
    );
    files.insert(
        http.clone(),
        b"fn response() {}\nfn call() { response(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = named_in_file(&snap, "response", &http);
    let next = rename(&snap, id, "http_response").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let lib_txt = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    let http_txt = String::from_utf8(rendered.files[&http].clone()).unwrap();
    assert!(
        lib_txt.contains("use axum::response::{Html, IntoResponse};"),
        "{lib_txt}"
    );
    assert!(!lib_txt.contains("http_response"), "{lib_txt}");
    assert!(http_txt.contains("fn http_response"), "{http_txt}");
    assert!(http_txt.contains("http_response();"), "{http_txt}");
}

#[test]
fn rename_same_named_fn_in_the_same_crate_rewrites_only_that_file() {
    let store = MemStore::new();
    let langs = rust_langs();
    let engine = RelPath::new("crates/svc-core/src/engine/mod.rs").unwrap();
    let merge = RelPath::new("crates/svc-core/src/engine/merge.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        engine.clone(),
        b"fn resolve() {}\nfn go() { resolve(); }\n".to_vec(),
    );
    files.insert(
        merge.clone(),
        b"fn resolve() {}\nfn other() { resolve(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = named_in_file(&snap, "resolve", &engine);
    let next = rename(&snap, id, "resolve_renamed").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let engine_txt = String::from_utf8(rendered.files[&engine].clone()).unwrap();
    let merge_txt = String::from_utf8(rendered.files[&merge].clone()).unwrap();
    assert!(engine_txt.contains("fn resolve_renamed"), "{engine_txt}");
    assert!(engine_txt.contains("resolve_renamed();"), "{engine_txt}");
    assert!(!engine_txt.contains("fn resolve("), "{engine_txt}");
    assert!(merge_txt.contains("fn resolve()"), "{merge_txt}");
    assert!(merge_txt.contains("resolve();"), "{merge_txt}");
    assert!(!merge_txt.contains("resolve_renamed"), "{merge_txt}");
}

#[test]
fn absorb_does_not_bind_a_deleted_same_file_def() {
    let store = MemStore::new();
    let langs = rust_langs();
    let main = RelPath::new("crates/svc/src/main.rs").unwrap();
    let repo = RelPath::new("crates/svc-repo/src/lib.rs").unwrap();
    let mut prev_files = BTreeMap::new();
    prev_files.insert(
        main.clone(),
        b"fn resolve_entity_in() {}\nfn show() { resolve_entity_in(); }\n".to_vec(),
    );
    prev_files.insert(repo.clone(), b"pub fn resolve_entity_in() {}\n".to_vec());
    let prev = snapshot_files(&store, &langs, &prev_files, None, ChangeId::new()).unwrap();
    let mut next_files = BTreeMap::new();
    next_files.insert(
        main.clone(),
        b"fn show() { resolve_entity_in(); }\n".to_vec(),
    );
    next_files.insert(repo.clone(), b"pub fn resolve_entity_in() {}\n".to_vec());
    let next = snapshot_files(&store, &langs, &next_files, Some(&prev), ChangeId::new()).unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&main].clone()).unwrap();
    assert!(
        !text.contains('?'),
        "deleted same-file def must not leave a hole: {text}"
    );
    assert!(text.contains("resolve_entity_in();"), "{text}");
    assert!(!text.contains("fn resolve_entity_in"), "{text}");
}
