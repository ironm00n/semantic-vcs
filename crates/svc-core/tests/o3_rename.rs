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

    let next = rename(&store, &snap, parse_id, "parse_config").unwrap();
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

    let next = rename(&store, &snap, method, "read_file").unwrap();
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

    let next = rename(&store, &snap, method, "create").unwrap();
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
    let next = rename(&store, &snap, method, "read_file").unwrap();
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
    let next = rename(&store, &snap, method, "create").unwrap();
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

    let renamed_free = rename(&store, &snap, free, "fetch").unwrap();
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

    let renamed_method = rename(&store, &snap, method, "pull").unwrap();
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
    let next = rename(&store, &snap, repo_id, "hex_of_id").unwrap();
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
    let next = rename(&store, &snap, id, "http_response").unwrap();
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
fn rename_follows_bare_use_and_use_as() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(a.clone(), b"pub fn f() {}\npub fn g() {}\npub fn h() {}\n".to_vec());
    files.insert(
        lib.clone(),
        b"mod a;\nuse crate::a::f;\nuse crate::a::{g, h as hh};\nuse crate::a::g as gg;\nfn main() { f(); g(); hh(); gg(); }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let f = named_in_file(&snap, "f", &a);
    let g = named_in_file(&snap, "g", &a);
    let h = named_in_file(&snap, "h", &a);
    let after_f = rename(&store, &snap, f, "f2").unwrap();
    let after_g = rename(&store, &after_f, g, "g2").unwrap();
    let next = rename(&store, &after_g, h, "h2").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(text.contains("use crate::a::f2;"), "{text}");
    assert!(text.contains("use crate::a::{g2, h2 as hh};"), "{text}");
    assert!(text.contains("use crate::a::g2 as gg;"), "{text}");
    assert!(text.contains("f2(); g2(); hh(); gg();"), "{text}");
    assert!(!text.contains("use crate::a::f;"), "{text}");
    assert!(!text.contains("use crate::a::g as gg;"), "{text}");
}

#[test]
fn rename_follows_super_path_use() {
    let store = MemStore::new();
    let langs = rust_langs();
    let diff_impl = RelPath::new("crates/pkg/src/diff_impl.rs").unwrap();
    let ops = RelPath::new("crates/pkg/src/ops.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(diff_impl.clone(), b"pub fn lang_for_ext() {}\n".to_vec());
    files.insert(
        ops.clone(),
        b"use super::diff_impl::lang_for_ext;\nfn go() { lang_for_ext(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = named_in_file(&snap, "lang_for_ext", &diff_impl);
    let next = rename(&store, &snap, id, "lang_of_extension").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&ops].clone()).unwrap();
    assert!(
        text.contains("use super::diff_impl::lang_of_extension;"),
        "{text}"
    );
    assert!(text.contains("lang_of_extension();"), "{text}");
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
    let next = rename(&store, &snap, id, "resolve_renamed").unwrap();
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
fn unique_crate_name_rewrites_a_sibling_file() {
    let store = MemStore::new();
    let langs = rust_langs();
    let a = RelPath::new("crates/pkg/src/a.rs").unwrap();
    let b = RelPath::new("crates/pkg/src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(a.clone(), b"fn helper() {}\n".to_vec());
    files.insert(b.clone(), b"fn go() { helper(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = named_in_file(&snap, "helper", &a);
    let next = rename(&store, &snap, id, "helper_renamed").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let b_txt = String::from_utf8(rendered.files[&b].clone()).unwrap();
    assert!(b_txt.contains("helper_renamed();"), "{b_txt}");
}

#[test]
fn ambiguous_crate_name_is_free_in_a_third_file() {
    let store = MemStore::new();
    let langs = rust_langs();
    let a = RelPath::new("crates/pkg/src/a.rs").unwrap();
    let b = RelPath::new("crates/pkg/src/b.rs").unwrap();
    let c = RelPath::new("crates/pkg/src/c.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    files.insert(b.clone(), b"fn parse() {}\n".to_vec());
    files.insert(c.clone(), b"fn go() { parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = named_in_file(&snap, "parse", &b);
    let next = rename(&store, &snap, id, "parse_b").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let c_txt = String::from_utf8(rendered.files[&c].clone()).unwrap();
    assert!(c_txt.contains("parse();"), "{c_txt}");
    assert!(!c_txt.contains("parse_b"), "{c_txt}");
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

#[test]
fn rename_of_a_nested_fn_rewrites_the_parent_call_not_a_sibling() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn f() { fn g() {}\n g(); }\nfn h() { g(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let nested = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "g" && r.parent.is_some())
        .map(|(id, _)| *id)
        .expect("nested g");
    let next = rename(&store, &snap, nested, "helper").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn helper()"), "{text}");
    assert!(text.contains("helper();"), "{text}");
    assert!(
        text.contains("fn h() { g(); }"),
        "sibling must keep the free g: {text}"
    );
    assert_eq!(
        text.matches("helper();").count(),
        1,
        "only the parent call rewrites: {text}"
    );
}

#[test]
fn nested_fn_shadows_a_file_level_fn_inside_the_parent() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn g() {}\nfn f() { fn g() {}\n g(); }\nfn h() { g(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let nested = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "g" && r.parent.is_some())
        .map(|(id, _)| *id)
        .expect("nested g");
    let file_g = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "g" && r.parent.is_none())
        .map(|(id, _)| *id)
        .expect("file g");
    let next = rename(&store, &snap, nested, "helper").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn helper()"), "{text}");
    assert!(text.contains("helper();"), "{text}");
    assert!(text.contains("fn g() {}"), "{text}");
    assert!(
        text.contains("fn h() { g(); }"),
        "file-level call must stay: {text}"
    );
    let next_file = rename(&store, &snap, file_g, "g_file").unwrap();
    let rendered = render(&next_file, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn g_file() {}"), "{text}");
    assert!(text.contains("fn h() { g_file(); }"), "{text}");
    assert!(
        text.contains("fn g() {}") && text.contains("g();"),
        "nested g and its call must not follow the file-level rename: {text}"
    );
}

#[test]
fn nested_fn_is_not_a_crate_name_collision() {
    let store = MemStore::new();
    let langs = rust_langs();
    let nested_file = RelPath::new("crates/svc/src/a.rs").unwrap();
    let def = RelPath::new("crates/svc/src/b.rs").unwrap();
    let use_file = RelPath::new("crates/svc/src/c.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        nested_file.clone(),
        b"fn wrap() { fn parse() {} parse(); }\n".to_vec(),
    );
    files.insert(def.clone(), b"fn parse() {}\n".to_vec());
    files.insert(use_file.clone(), b"fn go() { parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = named_in_file(&snap, "parse", &def);
    let next = rename(&store, &snap, id, "parse_b").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let c_txt = String::from_utf8(rendered.files[&use_file].clone()).unwrap();
    let a_txt = String::from_utf8(rendered.files[&nested_file].clone()).unwrap();
    assert!(c_txt.contains("parse_b();"), "{c_txt}");
    assert!(
        a_txt.contains("fn parse()") && a_txt.contains("parse();"),
        "nested parse stays: {a_txt}"
    );
}

#[test]
fn mod_tests_fn_is_not_a_crate_name_collision() {
    // M3: `#[cfg(test)] mod tests { fn parse() {} }` must not make a unique
    // crate-level `parse` look ambiguous to a third file.
    let store = MemStore::new();
    let langs = rust_langs();
    let tests_file = RelPath::new("crates/svc/src/a.rs").unwrap();
    let def = RelPath::new("crates/svc/src/b.rs").unwrap();
    let use_file = RelPath::new("crates/svc/src/c.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        tests_file.clone(),
        b"#[cfg(test)]\nmod tests {\n    fn parse() {}\n    fn t() { parse(); }\n}\n".to_vec(),
    );
    files.insert(def.clone(), b"fn parse() {}\n".to_vec());
    files.insert(use_file.clone(), b"fn go() { parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = named_in_file(&snap, "parse", &def);
    let next = rename(&store, &snap, id, "parse_b").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let c_txt = String::from_utf8(rendered.files[&use_file].clone()).unwrap();
    let a_txt = String::from_utf8(rendered.files[&tests_file].clone()).unwrap();
    assert!(c_txt.contains("parse_b();"), "{c_txt}");
    assert!(
        a_txt.contains("fn parse()") && a_txt.contains("parse();"),
        "mod tests parse stays: {a_txt}"
    );
    assert!(!a_txt.contains("parse_b"), "{a_txt}");
}

#[test]
fn rename_of_a_mod_tests_fn_rewrites_the_sibling_not_a_file_level_call() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn parse() {}\n#[cfg(test)]\nmod tests {\n    fn parse() {}\n    fn t() { parse(); }\n}\nfn go() { parse(); }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let nested = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.parent.is_some())
        .map(|(id, _)| *id)
        .expect("mod tests parse");
    let next = rename(&store, &snap, nested, "parse_t").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn parse_t()"), "{text}");
    assert!(text.contains("fn t() { parse_t(); }"), "{text}");
    assert!(
        text.contains("fn parse() {}") && text.contains("fn go() { parse(); }"),
        "file-level parse must stay: {text}"
    );
}

#[test]
fn type_path_open_is_not_the_free_fn() {
    // Adding a file-level `fn open` must not rebind `RedbStore::open()` in
    // another file. `crate::open()` still follows the unique crate fn.
    let store = MemStore::new();
    let langs = rust_langs();
    let store_rs = RelPath::new("crates/svc-repo/src/store.rs").unwrap();
    let sync_rs = RelPath::new("crates/svc-repo/src/sync.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        store_rs.clone(),
        b"pub struct RedbStore;\nimpl RedbStore {\n    pub fn open() -> RedbStore { RedbStore }\n}\npub fn open_with() {\n    let _ = RedbStore::open();\n}\npub fn via_crate() {\n    crate::open();\n}\n"
            .to_vec(),
    );
    files.insert(sync_rs.clone(), b"pub fn helper() {}\n".to_vec());
    let before = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let open_with = named_in_file(&before, "open_with", &store_rs);
    files.insert(sync_rs.clone(), b"pub fn open() {}\n".to_vec());
    let after = snapshot_files(&store, &langs, &files, Some(&before), ChangeId::new()).unwrap();
    assert_eq!(
        after.entities[&open_with].content, before.entities[&open_with].content,
        "RedbStore::open must not rebind to the new fn"
    );
    assert_eq!(
        after.entities[&open_with].bytes, before.entities[&open_with].bytes
    );
    let open_id = named_in_file(&after, "open", &sync_rs);
    let content = store
        .get_content(after.entities[&open_with].content)
        .unwrap();
    assert!(
        !content.tokens.iter().any(|t| matches!(t, Token::Ident(IdentRef::Entity(id)) if *id == open_id)),
        "type path must stay Free, got {:?}",
        content.tokens
    );
    let via = named_in_file(&after, "via_crate", &store_rs);
    let via_c = store.get_content(after.entities[&via].content).unwrap();
    assert!(
        via_c
            .tokens
            .iter()
            .any(|t| matches!(t, Token::Ident(IdentRef::Entity(id)) if *id == open_id)),
        "crate::open still binds, got {:?}",
        via_c.tokens
    );
    let renamed = rename(&store, &after, open_id, "open_store").unwrap();
    let rendered = render(&renamed, &store, &langs, false).unwrap();
    let store_txt = String::from_utf8(rendered.files[&store_rs].clone()).unwrap();
    let sync_txt = String::from_utf8(rendered.files[&sync_rs].clone()).unwrap();
    assert!(store_txt.contains("RedbStore::open()"), "{store_txt}");
    assert!(!store_txt.contains("RedbStore::open_store()"), "{store_txt}");
    assert!(store_txt.contains("crate::open_store()"), "{store_txt}");
    assert!(sync_txt.contains("fn open_store"), "{sync_txt}");
}

#[test]
fn same_impl_type_path_open_is_not_the_free_fn() {
    // `S::open` inside `impl S` is still type-relative when `open` is not an
    // associated item. The enclosing-impl exemption for `S::Item` must not
    // bind it to a free `fn open`.
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn open() {}\nstruct S;\nimpl S {\n    fn f() { let _ = S::open(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let open = named_in_file(&snap, "open", &path);
    let next = rename(&store, &snap, open, "opened").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn opened()"), "{text}");
    assert!(
        text.contains("S::open()"),
        "type path must stay Free: {text}"
    );
    assert!(!text.contains("S::opened()"), "{text}");
}

#[test]
fn type_binding_name_is_not_a_free_type_alias() {
    // `It<Item = Item>`: the left `Item` names an associated type on `It`
    // (needs types, SPEC §9), the right `Item` is the file-level alias.
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"type Item = u8;\ntrait It { type Item; }\nfn f() -> impl It<Item = Item> { loop {} }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let alias = named_in_file(&snap, "Item", &path);
    let next = rename(&store, &snap, alias, "Elem").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("type Elem = u8"), "{text}");
    assert!(
        text.contains("It<Item = Elem>"),
        "type-binding name must stay: {text}"
    );
    assert!(!text.contains("It<Elem ="), "{text}");
}

#[test]
fn rename_of_const_does_not_rewrite_shorthand_field_name() {
    // `S { item }` is field `item` plus value `item`. Renaming the const must
    // not turn it into `S { ITEM }` (no such field).
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"const item: u8 = 1;\nstruct S { item: u8 }\nfn make() -> S { S { item } }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let c = named_in_file(&snap, "item", &path);
    let next = rename(&store, &snap, c, "ITEM").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("const ITEM: u8 = 1"), "{text}");
    assert!(
        text.contains("S { item: ITEM }") || text.contains("S { item:ITEM }"),
        "shorthand must expand so the field name stays: {text}"
    );
    assert!(!text.contains("S { ITEM }"), "{text}");
}

#[test]
fn shorthand_field_init_roundtrips_when_names_match() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"const item: u8 = 1;\nstruct S { item: u8 }\nfn make() -> S { S { item } }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let rendered = render(&snap, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("S { item }"),
        "matching names must stay shorthand: {text}"
    );
    assert!(!text.contains("S { item: item"), "{text}");
}

#[test]
fn rename_of_const_rewrites_const_block_not_the_shadowing_local() {
    // Locals do not enter a `const { }` (SPEC / rustc E0435). The `k` in the
    // const block is the file-level const, even when a local `k` shadows it
    // in the function body.
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"const k: u8 = 1;\nfn f() { let k = 2u8; const { let _ = k; } let _ = k; }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let c = named_in_file(&snap, "k", &path);
    let next = rename(&store, &snap, c, "key").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("const key: u8 = 1"), "{text}");
    assert!(
        text.contains("const { let _ = key; }"),
        "const block must see the file const: {text}"
    );
    assert!(text.contains("let k = 2u8"), "local must stay: {text}");
    assert!(
        text.contains("let _ = k;"),
        "use of the local must stay: {text}"
    );
}

#[test]
fn rename_of_fn_does_not_rewrite_struct_pattern_field() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn item() {}\nstruct S { item: u8 }\nfn f(s: S) { let S { item: x } = s; item(); }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let f = named_in_file(&snap, "item", &path);
    let next = rename(&store, &snap, f, "item2").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn item2()"), "{text}");
    assert!(text.contains("item2()"), "{text}");
    assert!(
        text.contains("let S { item: x }"),
        "pattern field name must stay: {text}"
    );
}

#[test]
fn rename_of_const_rewrites_array_length_not_the_shadowing_local() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"const n: usize = 1;\nfn f() { let n = 2usize; let _ = [0u8; n]; let _ = n; }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let c = named_in_file(&snap, "n", &path);
    let next = rename(&store, &snap, c, "nlen").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("const nlen: usize = 1"), "{text}");
    assert!(
        text.contains("[0u8; nlen]"),
        "array length must see the file const: {text}"
    );
    assert!(text.contains("let n = 2usize"), "{text}");
    assert!(text.contains("let _ = n;"), "{text}");
}

#[test]
fn rename_of_const_rewrites_unbraced_const_generic_not_the_shadowing_local() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"const n: usize = 1;\nfn g<const N: usize>() {}\nfn f() { let n = 2usize; g::<n>(); let _ = n; }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let c = named_in_file(&snap, "n", &path);
    let next = rename(&store, &snap, c, "nlen").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("const nlen: usize = 1"), "{text}");
    assert!(
        text.contains("g::<nlen>()"),
        "unbraced const generic arg must see the file const: {text}"
    );
    assert!(text.contains("let n = 2usize"), "{text}");
    assert!(text.contains("let _ = n;"), "{text}");
}

#[test]
fn rename_of_file_level_fn_does_not_rewrite_a_nested_mod_call() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn parse() {}\nmod inner {\n    fn f() { parse(); }\n}\nfn g() { parse(); }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let file_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.parent.is_none())
        .map(|(id, _)| *id)
        .expect("file-level parse");
    let next = rename(&store, &snap, file_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn parse_file()"), "{text}");
    assert!(text.contains("fn g() { parse_file(); }"), "{text}");
    assert!(
        text.contains("fn f() { parse(); }"),
        "nested mod must not see the outer item: {text}"
    );
}

#[test]
fn rename_follows_crate_path_inside_a_nested_mod() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn parse() {}\nmod inner {\n    fn f() { crate::parse(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let file_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.parent.is_none())
        .map(|(id, _)| *id)
        .expect("file-level parse");
    let next = rename(&store, &snap, file_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("crate::parse_file()"),
        "crate::parse inside the nested mod must follow: {text}"
    );
}

#[test]
fn rename_of_outer_mod_fn_does_not_rewrite_a_nested_mod_call() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod outer {\n    fn parse() {}\n    fn g() { parse(); }\n    mod inner {\n        fn f() { parse(); }\n    }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let outer_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "outer")
                })
        })
        .map(|(id, _)| *id)
        .expect("outer::parse");
    let next = rename(&store, &snap, outer_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(text.contains("fn parse_file()"), "{text}");
    assert!(text.contains("fn g() { parse_file(); }"), "{text}");
    assert!(
        text.contains("fn f() { parse(); }"),
        "inner mod must not see the outer mod item: {text}"
    );
}

#[test]
fn rename_follows_super_path_inside_a_nested_mod() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod outer {\n    fn parse() {}\n    mod inner {\n        fn f() { super::parse(); }\n    }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let outer_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "outer")
                })
        })
        .map(|(id, _)| *id)
        .expect("outer::parse");
    let next = rename(&store, &snap, outer_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("super::parse_file()"),
        "super::parse inside the nested mod must follow: {text}"
    );
}

#[test]
fn rename_of_mod_fn_rewrites_an_impl_method_call() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod outer {\n    fn parse() {}\n    struct S;\n    impl S {\n        fn f() { parse(); }\n    }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let outer_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "outer")
                })
        })
        .map(|(id, _)| *id)
        .expect("outer::parse");
    let next = rename(&store, &snap, outer_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("fn f() { parse_file(); }"),
        "impl method in the same mod must see the sibling: {text}"
    );
}

#[test]
fn rename_follows_self_path_inside_a_nested_mod() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod inner {\n    fn parse() {}\n    fn f() { self::parse(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let inner_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "inner")
                })
        })
        .map(|(id, _)| *id)
        .expect("inner::parse");
    let next = rename(&store, &snap, inner_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("self::parse_file()"),
        "self::parse inside the nested mod must follow: {text}"
    );
}

#[test]
fn rename_follows_super_super_path_inside_a_nested_mod() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod a {\n    fn parse() {}\n    mod b {\n        mod c {\n            fn f() { super::super::parse(); }\n        }\n    }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "a")
                })
        })
        .map(|(id, _)| *id)
        .expect("a::parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("super::super::parse_file()"),
        "super::super::parse must follow: {text}"
    );
}

#[test]
fn rename_of_file_level_fn_does_not_rewrite_a_crate_mod_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn parse() {}\nmod outer {\n    fn parse() {}\n}\nmod inner {\n    fn f() { crate::outer::parse(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let file_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.parent.is_none())
        .map(|(id, _)| *id)
        .expect("file-level parse");
    let next = rename(&store, &snap, file_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("crate::outer::parse()"),
        "crate::outer::parse is not the file-level fn: {text}"
    );
}

#[test]
fn rename_follows_crate_mod_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"fn parse() {}\nmod outer {\n    fn parse() {}\n}\nmod inner {\n    fn f() { crate::outer::parse(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let outer_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "outer")
                })
        })
        .map(|(id, _)| *id)
        .expect("outer::parse");
    let next = rename(&store, &snap, outer_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("crate::outer::parse_file()"),
        "crate::outer::parse must follow the nested item: {text}"
    );
}

#[test]
fn rename_follows_super_mod_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod a {\n    mod inner {\n        fn parse() {}\n    }\n    mod b {\n        fn f() { super::inner::parse(); }\n    }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let inner_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "inner")
                })
        })
        .map(|(id, _)| *id)
        .expect("inner::parse");
    let next = rename(&store, &snap, inner_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("super::inner::parse_file()"),
        "super::inner::parse must follow: {text}"
    );
}

#[test]
fn rename_follows_self_mod_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod a {\n    mod inner {\n        fn parse() {}\n    }\n    fn f() { self::inner::parse(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let inner_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "inner")
                })
        })
        .map(|(id, _)| *id)
        .expect("inner::parse");
    let next = rename(&store, &snap, inner_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("self::inner::parse_file()"),
        "self::inner::parse must follow: {text}"
    );
}

#[test]
fn rename_follows_crate_mod_mod_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let path = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        path.clone(),
        b"mod a {\n    mod b {\n        fn parse() {}\n    }\n}\nmod inner {\n    fn f() { crate::a::b::parse(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let b_parse = snap
        .entities
        .iter()
        .find(|(_, r)| {
            r.name == "parse"
                && r.parent.is_some_and(|p| {
                    snap.entities.get(&p).is_some_and(|pr| pr.name == "b")
                })
        })
        .map(|(id, _)| *id)
        .expect("a::b::parse");
    let next = rename(&store, &snap, b_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&path].clone()).unwrap();
    assert!(
        text.contains("crate::a::b::parse_file()"),
        "crate::a::b::parse must follow: {text}"
    );
}

#[test]
fn rename_follows_crate_path_into_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let outer = RelPath::new("src/outer.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"mod outer;\nmod inner {\n    fn f() { crate::outer::parse(); }\n}\n"
            .to_vec(),
    );
    files.insert(outer.clone(), b"fn parse() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let file_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == outer)
        .map(|(id, _)| *id)
        .expect("outer.rs parse");
    let next = rename(&store, &snap, file_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("crate::outer::parse_file()"),
        "crate::outer::parse should be the file module item: {text}"
    );
}

#[test]
fn rename_follows_crate_path_into_a_mod_rs_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let outer = RelPath::new("src/outer/mod.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"mod outer;\nfn f() { crate::outer::parse(); }\n".to_vec(),
    );
    files.insert(outer.clone(), b"fn parse() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let file_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == outer)
        .map(|(id, _)| *id)
        .expect("outer/mod.rs parse");
    let next = rename(&store, &snap, file_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("crate::outer::parse_file()"),
        "crate::outer::parse should be the mod.rs item: {text}"
    );
}

#[test]
fn rename_follows_super_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod foo;\n".to_vec());
    files.insert(foo.clone(), b"fn f() { super::parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("super::parse_file()"),
        "super::parse from a file module must follow: {text}"
    );
}

#[test]
fn rename_follows_crate_path_into_a_nested_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let outer = RelPath::new("src/outer.rs").unwrap();
    let inner = RelPath::new("src/outer/inner.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"mod outer;\nfn f() { crate::outer::inner::parse(); }\n".to_vec(),
    );
    files.insert(outer.clone(), b"mod inner;\n".to_vec());
    files.insert(inner.clone(), b"fn parse() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let inner_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == inner)
        .map(|(id, _)| *id)
        .expect("inner.rs parse");
    let next = rename(&store, &snap, inner_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("crate::outer::inner::parse_file()"),
        "crate::outer::inner::parse must follow the nested file module: {text}"
    );
}

#[test]
fn rename_of_lib_fn_does_not_rewrite_a_file_module_call() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod foo;\nfn g() { parse(); }\n".to_vec());
    files.insert(foo.clone(), b"fn f() { parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let lib_text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    let foo_text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(lib_text.contains("fn g() { parse_file(); }"), "{lib_text}");
    assert!(
        foo_text.contains("fn f() { parse(); }"),
        "file module must not see the crate-root item: {foo_text}"
    );
}

#[test]
fn rename_of_file_module_fn_rewrites_a_sibling() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod foo;\n".to_vec());
    files.insert(foo.clone(), b"fn parse() {}\nfn f() { parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let foo_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == foo)
        .map(|(id, _)| *id)
        .expect("foo parse");
    let next = rename(&store, &snap, foo_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("fn f() { parse_file(); }"),
        "siblings in the file module must still see each other: {text}"
    );
}

#[test]
fn rename_follows_super_super_from_a_nested_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let outer = RelPath::new("src/outer.rs").unwrap();
    let inner = RelPath::new("src/outer/inner.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod outer;\n".to_vec());
    files.insert(outer.clone(), b"fn parse() {}\nmod inner;\n".to_vec());
    files.insert(
        inner.clone(),
        b"fn f() { super::parse(); super::super::parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let outer_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == outer)
        .map(|(id, _)| *id)
        .expect("outer parse");
    let next = rename(&store, &snap, outer_parse, "parse_outer").unwrap();
    let next = rename(&store, &next, lib_parse, "parse_lib").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&inner].clone()).unwrap();
    assert!(
        text.contains("super::parse_outer()"),
        "one super is the parent file module: {text}"
    );
    assert!(
        text.contains("super::super::parse_lib()"),
        "two supers is the crate root: {text}"
    );
}

#[test]
fn rename_follows_self_path_in_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod foo;\n".to_vec());
    files.insert(foo.clone(), b"fn parse() {}\nfn f() { self::parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let foo_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == foo)
        .map(|(id, _)| *id)
        .expect("foo parse");
    let next = rename(&store, &snap, foo_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("self::parse_file()"),
        "self::parse in a file module must follow: {text}"
    );
}

#[test]
fn rename_follows_crate_engine_path_from_a_child_file() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("crates/pkg/src/lib.rs").unwrap();
    let engine = RelPath::new("crates/pkg/src/engine/mod.rs").unwrap();
    let merge = RelPath::new("crates/pkg/src/engine/merge.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod engine;\n".to_vec());
    files.insert(
        engine.clone(),
        b"fn snapshot_files() {}\nmod merge;\n".to_vec(),
    );
    files.insert(
        merge.clone(),
        b"fn f() { crate::engine::snapshot_files(); super::snapshot_files(); }\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "snapshot_files" && r.file == engine)
        .map(|(id, _)| *id)
        .expect("snapshot_files");
    let next = rename(&store, &snap, id, "snapshot_tree").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&merge].clone()).unwrap();
    assert!(
        text.contains("crate::engine::snapshot_tree()"),
        "crate::engine::snapshot_files from merge.rs must follow: {text}"
    );
    assert!(
        text.contains("super::snapshot_tree()"),
        "super::snapshot_files from merge.rs must follow: {text}"
    );
}

#[test]
fn rename_follows_use_super_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod foo;\n".to_vec());
    files.insert(
        foo.clone(),
        b"use super::parse;\nfn f() { parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("use super::parse_file;"),
        "use super::parse from a file module must follow: {text}"
    );
    assert!(text.contains("parse_file();"), "{text}");
}

#[test]
fn rename_follows_use_super_glob_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod foo;\n".to_vec());
    files.insert(foo.clone(), b"use super::*;\nfn f() { parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("parse_file();"),
        "use super::* from a file module must follow: {text}"
    );
}

#[test]
fn rename_follows_use_super_inside_an_inline_mod() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"fn parse() {}\nmod inner {\n    use super::parse;\n    fn f() { parse(); }\n}\n"
            .to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse")
        .map(|(id, _)| *id)
        .expect("parse");
    let next = rename(&store, &snap, parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("use super::parse_file;"),
        "use line inside inline mod must follow: {text}"
    );
    assert!(
        text.contains("parse_file();"),
        "body inside inline mod must follow the use: {text}"
    );
}

#[test]
fn rename_follows_use_crate_glob_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod foo;\n".to_vec());
    files.insert(foo.clone(), b"use crate::*;\nfn f() { parse(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("parse_file();"),
        "use crate::* from a file module must follow: {text}"
    );
}

#[test]
fn rename_follows_use_crate_engine_glob_from_a_child_file() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("crates/pkg/src/lib.rs").unwrap();
    let engine = RelPath::new("crates/pkg/src/engine/mod.rs").unwrap();
    let merge = RelPath::new("crates/pkg/src/engine/merge.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod engine;\n".to_vec());
    files.insert(
        engine.clone(),
        b"fn snapshot_files() {}\nmod merge;\n".to_vec(),
    );
    files.insert(
        merge.clone(),
        b"use crate::engine::*;\nfn f() { snapshot_files(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "snapshot_files" && r.file == engine)
        .map(|(id, _)| *id)
        .expect("snapshot_files");
    let next = rename(&store, &snap, id, "snapshot_tree").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&merge].clone()).unwrap();
    assert!(
        text.contains("snapshot_tree();"),
        "use crate::engine::* from merge.rs must follow: {text}"
    );
}

#[test]
fn rename_follows_pub_use_reexport_as_crate_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let b = RelPath::new("src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod a;\nmod b;\npub use a::parse;\n".to_vec());
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    files.insert(
        b.clone(),
        b"fn parse() {}\nfn f() { crate::parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == a)
        .map(|(id, _)| *id)
        .expect("a parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let lib_text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    let b_text = String::from_utf8(rendered.files[&b].clone()).unwrap();
    assert!(
        lib_text.contains("pub use a::parse_file;"),
        "pub use path must follow: {lib_text}"
    );
    assert!(
        b_text.contains("crate::parse_file()"),
        "crate::parse via pub use must follow, not the local parse: {b_text}"
    );
    assert!(b_text.contains("fn parse()"), "{b_text}");
}

#[test]
fn rename_follows_crate_path_not_the_file_module_fn() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod foo;\n".to_vec());
    files.insert(
        foo.clone(),
        b"fn parse() {}\nfn f() { crate::parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("crate::parse_file()"),
        "crate::parse from a file module is the crate root, not the local parse: {text}"
    );
    assert!(text.contains("fn parse()"), "{text}");
}

#[test]
fn rename_follows_use_self_glob_in_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod foo;\n".to_vec());
    files.insert(
        foo.clone(),
        b"fn parse() {}\nuse self::*;\nfn f() { parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let foo_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == foo)
        .map(|(id, _)| *id)
        .expect("foo parse");
    let next = rename(&store, &snap, foo_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("fn parse_file()"),
        "def must rename: {text}"
    );
    assert!(
        text.contains("parse_file();"),
        "use self::* must keep the sibling in env: {text}"
    );
}

#[test]
fn rename_follows_relative_use_from_crate_root() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod a;\nuse a::parse;\nfn f() { parse(); }\n".to_vec());
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == a)
        .map(|(id, _)| *id)
        .expect("a parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("use a::parse_file;"),
        "relative use a::parse from crate root must follow: {text}"
    );
    assert!(text.contains("parse_file();"), "{text}");
}

#[test]
fn rename_follows_use_super_super_glob_from_a_nested_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let outer = RelPath::new("src/outer.rs").unwrap();
    let inner = RelPath::new("src/outer/inner.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod outer;\n".to_vec());
    files.insert(outer.clone(), b"mod inner;\n".to_vec());
    files.insert(
        inner.clone(),
        b"use super::super::*;\nfn f() { parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&inner].clone()).unwrap();
    assert!(
        text.contains("parse_file();"),
        "use super::super::* from a nested file module must follow: {text}"
    );
}

#[test]
fn rename_follows_crate_path_into_a_path_attr_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let bar = RelPath::new("src/bar.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"#[path = \"bar.rs\"]\nmod foo;\nfn f() { crate::foo::parse(); }\n".to_vec(),
    );
    files.insert(bar.clone(), b"fn parse() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == bar)
        .map(|(id, _)| *id)
        .expect("bar parse");
    let next = rename(&store, &snap, id, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("crate::foo::parse_file()"),
        "#[path] file module must follow: {text}"
    );
}

#[test]
fn rename_follows_pub_use_glob_as_crate_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let b = RelPath::new("src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod a;\nmod b;\npub use a::*;\n".to_vec());
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    files.insert(
        b.clone(),
        b"fn parse() {}\nfn f() { crate::parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == a)
        .map(|(id, _)| *id)
        .expect("a parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let b_text = String::from_utf8(rendered.files[&b].clone()).unwrap();
    assert!(
        b_text.contains("crate::parse_file()"),
        "pub use a::* must reexport parse: {b_text}"
    );
}

#[test]
fn rename_follows_crate_engine_merge_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("crates/pkg/src/lib.rs").unwrap();
    let engine = RelPath::new("crates/pkg/src/engine/mod.rs").unwrap();
    let merge = RelPath::new("crates/pkg/src/engine/merge.rs").unwrap();
    let ops = RelPath::new("crates/pkg/src/ops.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod engine;\nmod ops;\n".to_vec());
    files.insert(engine.clone(), b"mod merge;\n".to_vec());
    files.insert(merge.clone(), b"fn snapshot_files() {}\n".to_vec());
    files.insert(
        ops.clone(),
        b"fn f() { crate::engine::merge::snapshot_files(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "snapshot_files" && r.file == merge)
        .map(|(id, _)| *id)
        .expect("snapshot_files");
    let next = rename(&store, &snap, id, "snapshot_tree").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&ops].clone()).unwrap();
    assert!(
        text.contains("crate::engine::merge::snapshot_tree()"),
        "crate::engine::merge::snapshot_files must follow: {text}"
    );
}

#[test]
fn rename_follows_relative_use_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("crates/pkg/src/lib.rs").unwrap();
    let engine = RelPath::new("crates/pkg/src/engine/mod.rs").unwrap();
    let merge = RelPath::new("crates/pkg/src/engine/merge.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod engine;\n".to_vec());
    files.insert(
        engine.clone(),
        b"mod merge;\nuse merge::snapshot_files;\nfn f() { snapshot_files(); }\n".to_vec(),
    );
    files.insert(merge.clone(), b"fn snapshot_files() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "snapshot_files" && r.file == merge)
        .map(|(id, _)| *id)
        .expect("snapshot_files");
    let next = rename(&store, &snap, id, "snapshot_tree").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&engine].clone()).unwrap();
    assert!(
        text.contains("use merge::snapshot_tree;"),
        "relative use merge:: from engine/mod.rs must follow: {text}"
    );
    assert!(text.contains("snapshot_tree();"), "{text}");
}

#[test]
fn rename_follows_pub_crate_use_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nmod foo;\n".to_vec());
    files.insert(
        foo.clone(),
        b"pub(crate) use super::parse;\nfn f() { parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("pub(crate) use super::parse_file;"),
        "pub(crate) use must follow: {text}"
    );
    assert!(text.contains("parse_file();"), "{text}");
}

#[test]
fn rename_follows_pub_use_as_reexport() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let b = RelPath::new("src/b.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"mod a;\nmod b;\npub use a::parse as parse_a;\n".to_vec(),
    );
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    files.insert(b.clone(), b"fn f() { crate::parse_a(); }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == a)
        .map(|(id, _)| *id)
        .expect("a parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let lib_text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    let b_text = String::from_utf8(rendered.files[&b].clone()).unwrap();
    assert!(
        lib_text.contains("pub use a::parse_file as parse_a;"),
        "pub use as alias stays: {lib_text}"
    );
    assert!(
        b_text.contains("crate::parse_a()"),
        "crate::parse_a is the alias, not the new def name: {b_text}"
    );
}

#[test]
fn rename_follows_crate_path_into_a_nested_path_attr_file() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let bar = RelPath::new("src/nested/bar.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"#[path = \"nested/bar.rs\"]\nmod foo;\nfn f() { crate::foo::parse(); }\n".to_vec(),
    );
    files.insert(bar.clone(), b"fn parse() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == bar)
        .map(|(id, _)| *id)
        .expect("bar parse");
    let next = rename(&store, &snap, id, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("crate::foo::parse_file()"),
        "nested #[path] file module must follow: {text}"
    );
}

#[test]
fn rename_follows_use_super_list_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"fn parse() {}\nfn other() {}\nmod foo;\n".to_vec());
    files.insert(
        foo.clone(),
        b"use super::{parse, other};\nfn f() { parse(); other(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let lib_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == lib)
        .map(|(id, _)| *id)
        .expect("lib parse");
    let next = rename(&store, &snap, lib_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("use super::{parse_file, other};"),
        "use super list must follow: {text}"
    );
    assert!(text.contains("parse_file();"), "{text}");
}

#[test]
fn rename_follows_use_mod_as_then_path() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        lib.clone(),
        b"mod a;\nuse a as b;\nfn f() { b::parse(); }\n".to_vec(),
    );
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == a)
        .map(|(id, _)| *id)
        .expect("a parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("b::parse_file()"),
        "use a as b; b::parse must follow: {text}"
    );
}

#[test]
fn rename_follows_use_crate_mod_as_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod a;\nmod foo;\n".to_vec());
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    files.insert(
        foo.clone(),
        b"use crate::a as b;\nfn f() { b::parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == a)
        .map(|(id, _)| *id)
        .expect("a parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("b::parse_file()"),
        "use crate::a as b; b::parse from a file module must follow: {text}"
    );
}

#[test]
fn rename_follows_use_list_self_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let a = RelPath::new("src/a.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod a;\nmod foo;\n".to_vec());
    files.insert(a.clone(), b"fn parse() {}\n".to_vec());
    files.insert(
        foo.clone(),
        b"use crate::a::{self, parse};\nfn f() { a::parse(); parse(); }\n".to_vec(),
    );
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let a_parse = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == a)
        .map(|(id, _)| *id)
        .expect("a parse");
    let next = rename(&store, &snap, a_parse, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&foo].clone()).unwrap();
    assert!(
        text.contains("use crate::a::{self, parse_file};"),
        "use list self must keep the module and follow parse: {text}"
    );
    assert!(text.contains("a::parse_file()"), "{text}");
    assert!(text.contains("parse_file();"), "{text}");
}

#[test]
fn rename_follows_macro_rules_from_a_file_module() {
    let store = MemStore::new();
    let langs = rust_langs();
    let lib = RelPath::new("src/lib.rs").unwrap();
    let foo = RelPath::new("src/foo.rs").unwrap();
    let mut files = BTreeMap::new();
    files.insert(lib.clone(), b"mod foo;\nfn f() { parse!(); }\n".to_vec());
    files.insert(foo.clone(), b"macro_rules! parse { () => {}; }\n".to_vec());
    let snap = snapshot_files(&store, &langs, &files, None, ChangeId::new()).unwrap();
    let id = snap
        .entities
        .iter()
        .find(|(_, r)| r.name == "parse" && r.file == foo)
        .map(|(id, _)| *id)
        .expect("parse macro");
    let next = rename(&store, &snap, id, "parse_file").unwrap();
    let rendered = render(&next, &store, &langs, false).unwrap();
    let text = String::from_utf8(rendered.files[&lib].clone()).unwrap();
    assert!(
        text.contains("parse_file!();"),
        "macro_rules in a file module must follow at the crate root: {text}"
    );
}
